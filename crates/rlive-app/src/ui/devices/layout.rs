//! Where a device's controls go. Pure geometry: nothing here draws or reads the session.
//!
//! A device declares [`Section`]s of [`Item`]s. Sections sit side by side, and the items in a
//! section fill columns top to bottom, then wrap, so a panel reflows to the height of the detail
//! view. A device that declares nothing gets one untitled section of knobs, one per visible
//! parameter, which is what every device looked like before layouts existed.

use super::super::panels;
use egui::{pos2, vec2, Rect, Vec2};
use rlive_core::{Item, ParamFlags, ParamId, ParamInfo, Section};
use std::collections::HashMap;

pub const TITLE_H: f32 = 20.0;
pub const KNOB_W: f32 = 60.0;
pub const KNOB_H: f32 = 66.0;
pub const FOLDED_W: f32 = 24.0;
/// How many knobs an undeclared layout shows before asking.
pub const MAX_PARAMS: usize = 64;
pub const SLIDER_SIZE: Vec2 = vec2(120.0, 32.0);
pub const TOGGLE_SIZE: Vec2 = vec2(KNOB_W, 34.0);
pub const MENU_SIZE: Vec2 = vec2(96.0, 36.0);
pub const METER_SIZE: Vec2 = vec2(18.0, KNOB_H);
pub const SCOPE_SIZE: Vec2 = vec2(140.0, KNOB_H);
/// Room for a section's title. An untitled section takes none.
pub const SECTION_TITLE_H: f32 = 16.0;
pub const SECTION_GAP: f32 = 10.0;
const ROW_GAP: f32 = 4.0;
/// Width of everything around the body of a device card.
const CARD_MARGIN: f32 = 12.0;
const MIN_DEVICE_W: f32 = 150.0;
const MIN_PLUGIN_W: f32 = 170.0;

/// The parameters of a device by id, without the hidden ones.
pub type ParamIndex<'a> = HashMap<ParamId, &'a ParamInfo>;

pub fn param_index(params: &[ParamInfo]) -> ParamIndex<'_> {
    params
        .iter()
        .filter(|p| !p.flags.contains(ParamFlags::HIDDEN))
        .map(|p| (p.param_id(), p))
        .collect()
}

/// The layout of a device that declares none: its visible parameters as knobs, in order, cut
/// off at [`MAX_PARAMS`] unless `show_all`. Also says whether it had to cut any off.
pub fn auto_section(params: &[ParamInfo], show_all: bool) -> (Section, bool) {
    let visible = params
        .iter()
        .filter(|p| !p.flags.contains(ParamFlags::HIDDEN));
    let limit = if show_all { usize::MAX } else { MAX_PARAMS };
    let items: Vec<Item> = visible
        .clone()
        .take(limit)
        .map(|p| Item::Knob(p.param_id()))
        .collect();
    let truncated = visible.count() > items.len();
    (Section { title: "", items }, truncated)
}

/// The natural size of an item. A panel that isn't in the table gets the size of its placeholder.
pub fn item_size(item: &Item) -> Vec2 {
    match item {
        Item::Knob(_) => vec2(KNOB_W, KNOB_H),
        Item::Slider(_) => SLIDER_SIZE,
        Item::Toggle(_) => TOGGLE_SIZE,
        Item::Menu(_) => MENU_SIZE,
        Item::Meter(_) => METER_SIZE,
        Item::Scope(..) => SCOPE_SIZE,
        Item::Row(items) => {
            let gaps = items.len().saturating_sub(1) as f32 * ROW_GAP;
            items
                .iter()
                .map(item_size)
                .fold(vec2(gaps, 0.0), |s, i| vec2(s.x + i.x, s.y.max(i.y)))
        }
        Item::Panel(name) => panels::size(name),
    }
}

/// An item with its place, relative to the top-left of its section.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed<'a> {
    /// Never an [`Item::Row`]: a row is flattened into its children.
    pub item: Item,
    pub rect: Rect,
    /// What a parameter item controls. Always there for one that made it into a plan.
    pub param: Option<&'a ParamInfo>,
}

/// A section's items, flowed into columns.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionPlan<'a> {
    pub title: &'static str,
    pub items: Vec<Placed<'a>>,
    pub size: Vec2,
}

/// The parameter an item controls, if it controls one.
fn param_of(item: &Item) -> Option<ParamId> {
    match item {
        Item::Knob(p) | Item::Slider(p) | Item::Toggle(p) | Item::Menu(p) => Some(*p),
        _ => None,
    }
}

/// Whether an item can be drawn. A layout can name a parameter that is hidden or gone (a plugin
/// that rescanned, say), and that shouldn't leave a hole or take the UI down.
fn drawable(item: &Item, params: &ParamIndex) -> bool {
    match item {
        Item::Row(items) => items.iter().any(|i| drawable(i, params)),
        other => param_of(other).is_none_or(|p| params.contains_key(&p)),
    }
}

/// Lay out `section` to fit `avail_h`. An item taller than that gets a column to itself.
pub fn plan_section<'a>(
    section: &Section,
    avail_h: f32,
    params: &ParamIndex<'a>,
) -> SectionPlan<'a> {
    let top = if section.title.is_empty() {
        0.0
    } else {
        SECTION_TITLE_H
    };
    let usable = (avail_h - top).max(0.0);
    let mut items = Vec::new();
    let mut size = vec2(0.0, top);
    let mut column = vec2(0.0, 0.0);
    for item in section.items.iter().filter(|i| drawable(i, params)) {
        let s = fit_row(item, params);
        if column.y > 0.0 && column.y + s.y > usable {
            size.x += column.x;
            column = vec2(0.0, 0.0);
        }
        place(item, pos2(size.x, top + column.y), params, &mut items);
        column = vec2(column.x.max(s.x), column.y + s.y);
        size.y = size.y.max(top + column.y);
    }
    size.x += column.x;
    SectionPlan {
        title: section.title,
        items,
        size,
    }
}

/// The size of what remains of `item` once undrawable children are gone.
fn fit_row(item: &Item, params: &ParamIndex) -> Vec2 {
    match item {
        Item::Row(items) => {
            let kept: Vec<Vec2> = items
                .iter()
                .filter(|i| drawable(i, params))
                .map(|i| fit_row(i, params))
                .collect();
            let gaps = kept.len().saturating_sub(1) as f32 * ROW_GAP;
            kept.iter()
                .fold(vec2(gaps, 0.0), |s, i| vec2(s.x + i.x, s.y.max(i.y)))
        }
        other => item_size(other),
    }
}

/// Add `item` at `at`, flattening a row into its children.
fn place<'a>(item: &Item, at: egui::Pos2, params: &ParamIndex<'a>, out: &mut Vec<Placed<'a>>) {
    if let Item::Row(children) = item {
        let mut x = at.x;
        for child in children.iter().filter(|i| drawable(i, params)) {
            place(child, pos2(x, at.y), params, out);
            x += fit_row(child, params).x + ROW_GAP;
        }
        return;
    }
    out.push(Placed {
        item: item.clone(),
        rect: Rect::from_min_size(at, item_size(item)),
        param: param_of(item).and_then(|p| params.get(&p).copied()),
    });
}

/// How much of a card's height is left for sections, under the title bar and any rows above them.
pub fn body_height(height: f32, extra_h: f32) -> f32 {
    (height - TITLE_H - 10.0 - extra_h).max(KNOB_H)
}

/// Width of a device card holding these sections, side by side.
pub fn device_width(sections: &[SectionPlan], is_plugin: bool) -> f32 {
    let content: f32 = sections.iter().map(|s| s.size.x).sum();
    let gaps = sections.len().saturating_sub(1) as f32 * SECTION_GAP;
    let least = if is_plugin {
        MIN_PLUGIN_W
    } else {
        MIN_DEVICE_W
    };
    (content.max(KNOB_W) + gaps + CARD_MARGIN).max(least)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::{ScopeKind, ViewId};

    fn param(id: u32) -> ParamInfo {
        ParamInfo::continuous(id, "P", 0.0, 1.0, 0.0, "")
    }

    fn params(n: u32) -> Vec<ParamInfo> {
        (0..n).map(param).collect()
    }

    fn knob(id: u32) -> Item {
        Item::Knob(ParamId(id))
    }

    #[test]
    fn auto_layout_gives_every_visible_parameter_a_knob_in_order() {
        let mut ps = params(4);
        ps[1].flags = ParamFlags::HIDDEN;
        let (section, truncated) = auto_section(&ps, false);
        assert_eq!(section.items, [knob(0), knob(2), knob(3)]);
        assert_eq!(section.title, "");
        assert!(!truncated);
    }

    #[test]
    fn auto_layout_stops_at_the_limit_until_asked_for_all() {
        let ps = params(MAX_PARAMS as u32 + 3);
        let (few, truncated) = auto_section(&ps, false);
        assert_eq!(few.items.len(), MAX_PARAMS);
        assert!(truncated);
        let (all, truncated) = auto_section(&ps, true);
        assert_eq!(all.items.len(), ps.len());
        assert!(!truncated);
    }

    #[test]
    fn a_device_with_no_parameters_gets_an_empty_section() {
        let (section, truncated) = auto_section(&[], false);
        assert!(section.items.is_empty() && !truncated);
    }

    #[test]
    fn auto_layout_matches_the_old_knob_grid() {
        // Column-major, and the same width the flat grid had at every panel height.
        let ps = params(10);
        let index = param_index(&ps);
        let (section, _) = auto_section(&ps, false);
        for rows in 1..=4usize {
            let plan = plan_section(&section, rows as f32 * KNOB_H + 3.0, &index);
            for (i, placed) in plan.items.iter().enumerate() {
                let expect = pos2((i / rows) as f32 * KNOB_W, (i % rows) as f32 * KNOB_H);
                assert_eq!(placed.rect.min, expect, "knob {i} with {rows} rows");
            }
            let cols = 10usize.div_ceil(rows);
            assert_eq!(device_width(&[plan], false), cols as f32 * KNOB_W + 12.0);
        }
    }

    #[test]
    fn device_width_has_a_floor_and_plugins_get_room_for_their_button() {
        assert_eq!(device_width(&[], false), 150.0);
        assert_eq!(device_width(&[], true), 170.0);
    }

    #[test]
    fn sections_sit_side_by_side_with_a_gap() {
        let ps = params(2);
        let index = param_index(&ps);
        let a = Section {
            title: "A",
            items: vec![knob(0)],
        };
        let b = Section {
            title: "B",
            items: vec![knob(1)],
        };
        let plans = [
            plan_section(&a, 200.0, &index),
            plan_section(&b, 200.0, &index),
        ];
        assert_eq!(
            device_width(&plans, false),
            (2.0 * KNOB_W + SECTION_GAP + 12.0).max(150.0)
        );
    }

    #[test]
    fn a_title_takes_room_from_the_column() {
        let ps = params(2);
        let index = param_index(&ps);
        let section = Section {
            title: "Filter",
            items: vec![knob(0), knob(1)],
        };
        // Two knobs fit untitled at this height, but not with the title.
        let plan = plan_section(&section, 2.0 * KNOB_H + 4.0, &index);
        assert_eq!(plan.items[0].rect.min, pos2(0.0, SECTION_TITLE_H));
        assert_eq!(plan.items[1].rect.min, pos2(KNOB_W, SECTION_TITLE_H));
        assert_eq!(plan.size, vec2(2.0 * KNOB_W, SECTION_TITLE_H + KNOB_H));
    }

    #[test]
    fn a_column_breaks_only_when_it_is_full() {
        let ps = params(3);
        let index = param_index(&ps);
        let section = Section {
            title: "",
            items: vec![knob(0), knob(1), knob(2)],
        };
        let plan = plan_section(&section, 2.0 * KNOB_H, &index);
        assert_eq!(plan.items[1].rect.min, pos2(0.0, KNOB_H));
        assert_eq!(plan.items[2].rect.min, pos2(KNOB_W, 0.0));
    }

    #[test]
    fn an_item_taller_than_the_panel_still_gets_placed() {
        let ps = params(1);
        let index = param_index(&ps);
        let section = Section {
            title: "",
            items: vec![knob(0)],
        };
        let plan = plan_section(&section, 10.0, &index);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.size, vec2(KNOB_W, KNOB_H));
    }

    #[test]
    fn a_row_lays_its_children_out_across() {
        let ps = params(2);
        let index = param_index(&ps);
        let section = Section {
            title: "",
            items: vec![Item::Row(vec![knob(0), Item::Meter(ViewId(0)), knob(1)])],
        };
        let plan = plan_section(&section, 200.0, &index);
        let xs: Vec<f32> = plan.items.iter().map(|p| p.rect.min.x).collect();
        assert_eq!(
            xs,
            [0.0, KNOB_W + ROW_GAP, KNOB_W + 2.0 * ROW_GAP + METER_SIZE.x]
        );
        assert_eq!(
            plan.size,
            vec2(2.0 * KNOB_W + METER_SIZE.x + 2.0 * ROW_GAP, KNOB_H)
        );
    }

    #[test]
    fn a_parameter_that_is_hidden_or_gone_is_left_out_without_a_hole() {
        let mut ps = params(2);
        ps[0].flags = ParamFlags::HIDDEN;
        let index = param_index(&ps);
        let section = Section {
            title: "",
            items: vec![
                knob(0),
                knob(9),
                Item::Row(vec![knob(9)]),
                Item::Row(vec![knob(9), knob(1)]),
            ],
        };
        let plan = plan_section(&section, 200.0, &index);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].item, knob(1));
        assert_eq!(plan.items[0].rect.min, pos2(0.0, 0.0));
    }

    #[test]
    fn items_know_their_parameter() {
        let ps = params(1);
        let index = param_index(&ps);
        let section = Section {
            title: "",
            items: vec![knob(0), Item::Meter(ViewId(0))],
        };
        let plan = plan_section(&section, 200.0, &index);
        assert_eq!(plan.items[0].param.map(|p| p.id), Some(0));
        assert!(plan.items[1].param.is_none());
    }

    #[test]
    fn natural_sizes_follow_the_item_kind() {
        assert_eq!(
            item_size(&Item::Scope(ViewId(0), ScopeKind::Spectrum)),
            SCOPE_SIZE
        );
        assert_eq!(item_size(&Item::Slider(ParamId(0))), SLIDER_SIZE);
        assert_eq!(item_size(&Item::Toggle(ParamId(0))), TOGGLE_SIZE);
        assert_eq!(item_size(&Item::Menu(ParamId(0))), MENU_SIZE);
        assert_eq!(item_size(&Item::Row(vec![])), Vec2::ZERO);
    }
}
