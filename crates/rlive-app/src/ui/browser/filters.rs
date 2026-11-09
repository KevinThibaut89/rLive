//! The Filter View, Tag Editor and Quick Tags.

use super::super::theme::{self, *};
use super::super::widgets::info;
use super::super::UiState;
use egui::{Align2, Rect, Sense, Vec2};
use rlive_browser::item::ItemKey;
use rlive_browser::tags::{GroupId, Tag, TagId};
use rlive_browser::{LibraryData, Op};
use std::collections::BTreeSet;
use std::sync::Arc;

pub(super) const QUICK_H: f32 = 44.0;

fn library(state: &UiState) -> Arc<LibraryData> {
    state
        .browser
        .db
        .as_ref()
        .map_or_else(|| Arc::new(LibraryData::default()), |d| d.library().clone())
}

/// The Filter View menu (manual p. 73).
pub(super) fn menu(ui: &mut egui::Ui, state: &mut UiState) {
    let lib = library(state);
    let scope = state.browser.model.nav.scope.clone();
    let key = scope.storage_key();
    let visible: BTreeSet<GroupId> = lib.visible_groups(&scope).into_iter().collect();
    ui.label(egui::RichText::new("Filter groups").color(pal().text_dim));
    for g in lib.tags.all_groups() {
        let mut on = visible.contains(&g.id);
        if ui.checkbox(&mut on, g.name).changed() {
            let mut groups = visible.clone();
            if on {
                groups.insert(g.id);
            } else {
                groups.remove(&g.id);
            }
            state
                .browser
                .op(Op::SetFilterGroups(key.clone(), Some(groups)));
        }
    }
    if ui.button("Reset Filter Groups to Default").clicked() {
        state.browser.op(Op::SetFilterGroups(key.clone(), None));
        ui.close();
    }
    ui.separator();
    let mut quick = lib.show_quick_tags;
    if ui.checkbox(&mut quick, "Open Quick Tags").changed() {
        state.browser.op(Op::SetShowQuickTags(quick));
    }
    let mut editor = lib.show_tag_editor;
    if ui
        .checkbox(&mut editor, "Open Tag Editor (Cmd+Shift+E)")
        .changed()
    {
        state.browser.op(Op::SetShowTagEditor(editor));
    }
    let mut auto = lib.auto_tags_enabled;
    ui.add_enabled_ui(false, |ui| {
        ui.checkbox(&mut auto, "Enable Auto Tags")
            .on_disabled_hover_text("Auto tags from sound analysis arrive with A19");
    });
}

/// Every selected row's key and its factory tags.
fn selection(state: &UiState) -> Vec<(ItemKey, Vec<TagId>)> {
    state
        .browser
        .model
        .selected_rows()
        .iter()
        .map(|r| (r.item.key.clone(), r.item.factory_tags.clone()))
        .collect()
}

fn chip(ui: &mut egui::Ui, text: &str, on: bool) -> egui::Response {
    let font = theme::font(10.0);
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        font,
        if on { shade(0xffffff) } else { pal().text },
    );
    let size = galley.size() + Vec2::new(9.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if on {
        pal().accent
    } else if resp.hovered() {
        pal().raised
    } else {
        pal().field_bg
    };
    ui.painter().rect_filled(rect, 2.0, fill);
    ui.painter()
        .galley(rect.min + Vec2::new(4.5, 2.0), galley, pal().text);
    resp
}

/// The filter groups and their tags.
pub(super) fn filter_view(ui: &mut egui::Ui, state: &mut UiState) {
    let lib = library(state);
    let scope = state.browser.model.nav.scope.clone();
    let groups = lib.visible_groups(&scope);
    ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
    for group_id in groups {
        let Some(group) = lib.tags.group(group_id) else {
            continue;
        };
        ui.add_space(2.0);
        let header = ui.add(
            egui::Label::new(
                egui::RichText::new(group.name.to_uppercase())
                    .font(theme::font_bold(9.0))
                    .color(pal().text_faint),
            )
            .sense(Sense::click()),
        );
        header.context_menu(|ui| {
            if ui.button("Hide Filter Group").clicked() {
                let mut shown: BTreeSet<GroupId> = lib.visible_groups(&scope).into_iter().collect();
                shown.remove(&group_id);
                state
                    .browser
                    .op(Op::SetFilterGroups(scope.storage_key(), Some(shown)));
                ui.close();
            }
        });
        let tags = lib.tags.tags_of(group_id);
        let in_group: Vec<TagId> = tags.iter().map(|t| t.id).collect();
        ui.horizontal_wrapped(|ui| {
            for tag in tags.iter().filter(|t| t.parent.is_none()) {
                let on = state.browser.model.nav.tags.contains(&tag.id);
                let r = chip(ui, &tag.name, on);
                if info(
                    r,
                    format!(
                        "{} — only show what's tagged this; Cmd+click to pick several",
                        tag.name
                    ),
                )
                .clicked()
                {
                    let multi = ui.input(|i| i.modifiers.command);
                    state.browser.model.click_tag(tag.id, multi, &in_group);
                    let selected = state.browser.model.nav.tags.clone();
                    state
                        .browser
                        .op(Op::SetFilterSelection(scope.storage_key(), selected));
                }
            }
        });
    }
    if lib.tags.all_groups().is_empty() || lib.visible_groups(&scope).is_empty() {
        ui.label(egui::RichText::new("No filter groups shown").color(pal().text_dim));
    }
}

/// The Quick Tags section (manual p. 79).
pub(super) fn quick_tags(ui: &mut egui::Ui, rect: Rect, state: &mut UiState) {
    let lib = library(state);
    let sel = selection(state);
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0, pal().separator),
    );
    let mut qu = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(4.0, 2.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true)),
    );
    qu.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
    if sel.is_empty() {
        qu.label(egui::RichText::new("Select something to tag it").color(pal().text_faint));
        return;
    }
    let items: Vec<ItemKey> = sel.iter().map(|(k, _)| k.clone()).collect();
    for q in lib.tags.quick_tags(&sel) {
        let text = if q.partial {
            format!("{} *", q.name)
        } else {
            q.name.clone()
        };
        let r = chip(&mut qu, &text, false);
        let group = lib.tags.group_of(q.tag).map_or(String::new(), |g| g.name);
        if r.clicked() {
            state.status = format!("Tag group: {group}");
        }
        let r = info(r, format!("{} — in the {} group", q.name, group));
        if q.removable {
            let x =
                Rect::from_center_size(r.rect.right_top() + Vec2::new(-2.0, 2.0), Vec2::splat(8.0));
            let xr = qu.interact(x, qu.scope_id().with(("quick_x", q.tag)), Sense::click());
            qu.painter().text(
                x.center(),
                Align2::CENTER_CENTER,
                "×",
                theme::font(9.0),
                if xr.hovered() {
                    pal().arm_red
                } else {
                    pal().text_dim
                },
            );
            if info(xr, "Remove this tag").clicked() {
                state.browser.op(Op::Assign {
                    items: items.clone(),
                    tag: q.tag,
                    on: false,
                });
            }
        }
    }
    quick_add(&mut qu, &lib, state, &items);
}

fn quick_add(ui: &mut egui::Ui, lib: &Arc<LibraryData>, state: &mut UiState, items: &[ItemKey]) {
    if state.browser.quick_new.is_some() {
        quick_parent_prompt(ui, lib, state, items);
        return;
    }
    let Some(text) = state.browser.quick_add.as_mut() else {
        if ui.small_button("Add…").clicked() {
            state.browser.quick_add = Some(String::new());
        }
        return;
    };
    let edit = ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(70.0)
            .hint_text("tag")
            .font(theme::font(10.0)),
    );
    edit.request_focus();
    let typed = text.clone();
    let suggestions: Vec<Tag> = if typed.is_empty() {
        Vec::new()
    } else {
        lib.tags.completions(&typed).into_iter().take(6).collect()
    };
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut chosen = None;
    if !suggestions.is_empty() {
        egui::Area::new(ui.scope_id().with("quick_suggest"))
            .order(egui::Order::Foreground)
            .fixed_pos(edit.rect.left_bottom())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    for t in &suggestions {
                        if ui.selectable_label(false, &t.name).clicked() {
                            chosen = Some(t.id);
                        }
                    }
                });
            });
    }
    if esc {
        state.browser.quick_add = None;
        return;
    }
    if let Some(tag) = chosen {
        state.browser.op(Op::Assign {
            items: items.to_vec(),
            tag,
            on: true,
        });
        state.browser.quick_add = None;
        return;
    }
    if enter && !typed.trim().is_empty() {
        match suggestions
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(typed.trim()))
        {
            Some(t) => {
                state.browser.op(Op::Assign {
                    items: items.to_vec(),
                    tag: t.id,
                    on: true,
                });
                state.browser.quick_add = None;
            }
            None => {
                // Create new tag… asks where it goes.
                state.browser.quick_new = Some(typed.trim().to_string());
                state.browser.quick_new_parent.clear();
                state.browser.quick_add = None;
            }
        }
    }
}

/// "Select a parent tag or group for the new tag" (manual p. 80).
fn quick_parent_prompt(
    ui: &mut egui::Ui,
    lib: &Arc<LibraryData>,
    state: &mut UiState,
    items: &[ItemKey],
) {
    let name = state.browser.quick_new.clone().unwrap_or_default();
    ui.label(egui::RichText::new(format!("Put “{name}” in:")).color(pal().text_dim));
    let r = ui.small_button("Choose…");
    egui::Popup::menu(&r).show(|ui| {
        for group in lib.tags.all_groups() {
            let tags = lib.tags.tags_of(group.id);
            ui.menu_button(group.name.clone(), |ui| {
                if ui.button("At top level").clicked() {
                    state.browser.op(Op::AddTag {
                        group: group.id,
                        parent: None,
                        name: name.clone(),
                        assign: items.to_vec(),
                    });
                    state.browser.quick_new = None;
                    ui.close();
                }
                for t in tags.iter().filter(|t| t.parent.is_none()) {
                    if ui.button(format!("Under {}", t.name)).clicked() {
                        state.browser.op(Op::AddTag {
                            group: group.id,
                            parent: Some(t.id),
                            name: name.clone(),
                            assign: items.to_vec(),
                        });
                        state.browser.quick_new = None;
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.browser.quick_new_parent)
                    .desired_width(80.0)
                    .hint_text("new group"),
            );
            if ui.button("Create").clicked() && !state.browser.quick_new_parent.trim().is_empty() {
                let group_name = state.browser.quick_new_parent.trim().to_string();
                state.browser.op(Op::AddGroup(group_name));
                ui.close();
            }
        });
    });
    if ui.small_button("Cancel").clicked() {
        state.browser.quick_new = None;
    }
}

/// The Tag Editor (manual pp. 75-79), which tags whatever is selected in the content pane.
pub(super) fn tag_editor_window(ctx: &egui::Context, state: &mut UiState) {
    let lib = library(state);
    if !lib.show_tag_editor {
        return;
    }
    let mut open = true;
    egui::Window::new("Tag Editor")
        .id(egui::Id::unique("rlive_tag_editor"))
        .default_size([260.0, 380.0])
        .default_pos([334.0, 60.0])
        .open(&mut open)
        .show(ctx, |ui| tag_editor(ui, &lib, state));
    if !open {
        state.browser.op(Op::SetShowTagEditor(false));
    }
}

fn tag_editor(ui: &mut egui::Ui, lib: &Arc<LibraryData>, state: &mut UiState) {
    let sel = selection(state);
    let items: Vec<ItemKey> = sel.iter().map(|(k, _)| k.clone()).collect();
    let on_all =
        |lib: &LibraryData, t: TagId| sel.iter().all(|(k, f)| lib.tags_of(k, f).contains(&t));
    let on_any =
        |lib: &LibraryData, t: TagId| sel.iter().any(|(k, f)| lib.tags_of(k, f).contains(&t));
    ui.label(
        egui::RichText::new(if sel.is_empty() {
            "Select items in the browser".to_string()
        } else if sel.len() == 1 {
            sel[0].0 .0.clone()
        } else {
            format!("{} items selected", sel.len())
        })
        .color(pal().text_dim),
    );
    egui::ScrollArea::vertical().show(ui, |ui| {
        for group in lib.tags.all_groups() {
            let tags = lib.tags.tags_of(group.id);
            let any_used = tags.iter().any(|t| on_any(lib, t.id));
            let folded = state.browser.tag_fold.contains(&group.id.0);
            let title = format!("{} {}", if folded { "▶" } else { "▼" }, group.name);
            let title = if any_used && folded {
                egui::RichText::new(title).strong().color(pal().accent)
            } else {
                egui::RichText::new(title).strong()
            };
            let r = ui.add(egui::Label::new(title).sense(Sense::click()));
            r.context_menu(|ui| {
                if !group.factory {
                    if ui.button("Delete Group").clicked() {
                        state.browser.op(Op::DeleteGroup(group.id));
                        ui.close();
                    }
                    ui.horizontal(|ui| {
                        let text = state.browser.tag_new.entry(group.id.0 + 1).or_default();
                        ui.add(
                            egui::TextEdit::singleline(text)
                                .desired_width(90.0)
                                .hint_text("rename"),
                        );
                        if ui.button("Rename").clicked() {
                            let name = text.trim().to_string();
                            state.browser.op(Op::RenameGroup(group.id, name));
                            ui.close();
                        }
                    });
                } else {
                    ui.label("Factory groups can't be changed");
                }
            });
            if r.clicked() && !state.browser.tag_fold.remove(&group.id.0) {
                state.browser.tag_fold.insert(group.id.0);
            }
            if folded {
                continue;
            }
            tag_rows(ui, &tags, None, 0, lib, state, &items, &on_all);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let text = state.browser.tag_new.entry(group.id.0).or_default();
                let edit = ui.add(
                    egui::TextEdit::singleline(text)
                        .desired_width(110.0)
                        .hint_text("Add Tag…")
                        .font(theme::font(10.0)),
                );
                let add = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if add && !text.trim().is_empty() {
                    let name = text.trim().to_string();
                    text.clear();
                    state.browser.op(Op::AddTag {
                        group: group.id,
                        parent: None,
                        name,
                        assign: Vec::new(),
                    });
                }
            });
            ui.separator();
        }
        ui.horizontal(|ui| {
            let text = state.browser.tag_new.entry(0).or_default();
            let edit = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(130.0)
                    .hint_text("Add Group…")
                    .font(theme::font(10.0)),
            );
            if edit.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !text.trim().is_empty()
            {
                let name = text.trim().to_string();
                text.clear();
                state.browser.op(Op::AddGroup(name));
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn tag_rows(
    ui: &mut egui::Ui,
    tags: &[Tag],
    parent: Option<TagId>,
    depth: usize,
    lib: &Arc<LibraryData>,
    state: &mut UiState,
    items: &[ItemKey],
    on_all: &dyn Fn(&LibraryData, TagId) -> bool,
) {
    for tag in tags.iter().filter(|t| t.parent == parent) {
        let mut checked = on_all(lib, tag.id);
        let r = ui.horizontal(|ui| {
            ui.add_space(12.0 + depth as f32 * 12.0);
            let enabled = !items.is_empty();
            ui.add_enabled(enabled, egui::Checkbox::new(&mut checked, tag.name.clone()))
        });
        let resp = r.inner;
        if resp.changed() {
            state.browser.op(Op::Assign {
                items: items.to_vec(),
                tag: tag.id,
                on: checked,
            });
        }
        let id = tag.id;
        let (is_factory, group) = (tag.factory, tag.group);
        resp.context_menu(|ui| {
            let text = state
                .browser
                .tag_new
                .entry(id.0.wrapping_add(2))
                .or_default();
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(text)
                        .desired_width(90.0)
                        .hint_text("subtag"),
                );
            });
            if ui.button("Add Subtag").clicked() {
                let name = text.trim().to_string();
                text.clear();
                state.browser.op(Op::AddTag {
                    group,
                    parent: Some(id),
                    name,
                    assign: Vec::new(),
                });
                ui.close();
            }
            if !is_factory {
                let text = state
                    .browser
                    .tag_new
                    .entry(id.0.wrapping_add(3))
                    .or_default();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(90.0)
                            .hint_text("rename"),
                    );
                });
                if ui.button("Rename").clicked() {
                    let name = text.trim().to_string();
                    state.browser.op(Op::RenameTag(id, name));
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    state.browser.op(Op::DeleteTag(id));
                    ui.close();
                }
            }
        });
        tag_rows(ui, tags, Some(tag.id), depth + 1, lib, state, items, on_all);
    }
}
