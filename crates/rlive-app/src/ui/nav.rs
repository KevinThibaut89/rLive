//! Keyboard focus for controls: where Tab goes, and how a focused control takes arrow keys and
//! typed values (manual pp. 1003-1013).
//!
//! Each control registers itself while it is drawn, so the Tab order is simply drawing order
//! within a focus area. Nothing here touches egui's own keyboard focus: a control with egui focus
//! silences every bare-key shortcut, and only the keys declared for [`When::ValueControl`] should
//! change meaning while a knob has the keyboard.
//!
//! [`When::ValueControl`]: super::keymap::When::ValueControl

use super::keymap::FocusArea;
use super::theme::pal;
use egui::{Align2, Context, Id, Rect, Response, Stroke, StrokeKind, Ui, Vec2};
use rlive_core::{DeviceId, TrackId};

/// What a control belongs to: Neighbor navigation moves between these.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Group {
    #[default]
    None,
    Track(TrackId),
    Device(DeviceId),
}

/// Something the keyboard asked a control to do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ValueOp {
    Step { up: bool, fine: bool },
    Default,
    Set(f64),
    Press,
}

/// One control that takes keyboard focus.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: Id,
    pub rect: Rect,
    pub area: FocusArea,
    pub group: Group,
    /// The same control on another track or device has the same role.
    pub role: Id,
    /// Takes typed values and arrow keys, rather than just being pressed.
    pub value: bool,
}

#[derive(Clone, Default)]
struct Data {
    current: Option<Id>,
    entry: Option<String>,
    pending: Option<(Id, ValueOp)>,
    area: FocusArea,
    group: Group,
    frame: Vec<Item>,
    last: Vec<Item>,
}

fn key() -> Id {
    Id::unique("rlive_nav")
}

fn with<R>(ctx: &Context, f: impl FnOnce(&mut Data) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<Data>(key())))
}

/// Call once a frame before anything draws. A press somewhere that isn't a control lets go of
/// the keyboard.
pub fn begin_frame(ctx: &Context) {
    let press = ctx.input(|i| {
        i.pointer
            .primary_pressed()
            .then(|| i.pointer.interact_pos())
    });
    with(ctx, |d| {
        d.last = std::mem::take(&mut d.frame);
        if let Some(pos) = press.flatten() {
            if !d.last.iter().any(|it| it.rect.contains(pos)) {
                d.current = None;
                d.entry = None;
            }
        }
        d.pending = None;
    });
}

/// The focus area of the controls drawn from here on.
pub fn set_area(ctx: &Context, area: FocusArea) {
    with(ctx, |d| {
        d.area = area;
        d.group = Group::None;
    });
}

/// The track or device the controls drawn from here on belong to.
pub fn set_group(ctx: &Context, group: Group) {
    with(ctx, |d| d.group = group);
}

/// How a control looks to the keyboard and to assistive technology.
pub struct Spec<'a> {
    /// Names the control among its group's, the same across groups.
    pub role: &'a str,
    /// What assistive technology reads out.
    pub label: &'a str,
    pub kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// Its current value, in the units the user sees.
    Value(f64),
    Button {
        on: bool,
    },
}

/// Registers `resp` for this frame and returns whatever the keyboard asked of it. A click also
/// gives the control the keyboard, so typing and the arrow keys work on what was just touched.
pub fn control(ui: &Ui, resp: &Response, spec: Spec) -> Option<ValueOp> {
    let (id, rect) = (resp.id, resp.rect);
    let value = matches!(spec.kind, Kind::Value(_));
    let mut out = None;
    let mut focused = false;
    let mut entry = None;
    let touched = resp.clicked() || resp.drag_started();
    with(ui.ctx(), |d| {
        let (area, group) = (d.area, d.group);
        d.frame.push(Item {
            id,
            rect,
            area,
            group,
            role: Id::unique(spec.role),
            value,
        });
        if touched {
            d.current = Some(id);
            d.entry = None;
        }
        focused = d.current == Some(id);
        if focused {
            entry = d.entry.clone();
            if let Some((target, op)) = d.pending {
                if target == id {
                    out = Some(op);
                    d.pending = None;
                }
            }
        }
    });
    // Announced by AccessKit when a screen reader is listening; a no-op otherwise.
    match spec.kind {
        Kind::Value(v) => {
            resp.widget_info(|| {
                egui::WidgetInfo::slider(ui.is_enabled(), v, spec.label.to_string())
            });
        }
        Kind::Button { on } => {
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::Role::Button,
                    ui.is_enabled(),
                    on,
                    spec.label.to_string(),
                )
            });
        }
    }
    if focused {
        let p = ui.painter();
        p.rect_stroke(
            rect.expand(1.0),
            1.0,
            Stroke::new(1.5, pal().accent),
            StrokeKind::Outside,
        );
        if let Some(text) = entry {
            // On top: a strip's own value readout is drawn after its controls.
            let layer = egui::LayerId::new(egui::Order::Tooltip, Id::unique("rlive_nav_entry"));
            let p = ui.ctx().layer_painter(layer);
            let tip = Rect::from_center_size(
                rect.center_bottom() + Vec2::new(0.0, 8.0),
                Vec2::new((text.len() as f32 * 7.0 + 8.0).max(40.0), 15.0),
            );
            p.rect_filled(tip, 1.0, pal().field_bg);
            p.rect_stroke(tip, 1.0, Stroke::new(1.0, pal().accent), StrokeKind::Inside);
            p.text(
                tip.center(),
                Align2::CENTER_CENTER,
                text,
                super::theme::font(11.0),
                pal().accent,
            );
        }
    }
    out
}

/// Whether a press or an arrow key means a button should change state.
pub fn toggled(op: Option<ValueOp>, on: bool) -> bool {
    match op {
        Some(ValueOp::Press) => true,
        Some(ValueOp::Step { up, .. }) => up != on,
        _ => false,
    }
}

/// The new value a value control should take, given its current one and how far one arrow press
/// moves it.
pub fn stepped(op: Option<ValueOp>, value: f64, step: f64, default: f64) -> Option<f64> {
    match op? {
        ValueOp::Step { up, fine } => {
            let d = if fine { step / 10.0 } else { step };
            Some(value + if up { d } else { -d })
        }
        ValueOp::Default => Some(default),
        ValueOp::Set(v) => Some(v),
        ValueOp::Press => None,
    }
}

// ---------------------------------------------------------------- moving the focus

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Next,
    Prev,
    NextNeighbor,
    PrevNeighbor,
}

/// The control focus goes to from `current`. Pure, so the order can be tested.
pub fn next_id(
    items: &[Item],
    current: Option<Id>,
    area: FocusArea,
    dir: Dir,
    wrap: bool,
) -> Option<Id> {
    let list: Vec<&Item> = items.iter().filter(|i| i.area == area).collect();
    if list.is_empty() {
        return None;
    }
    let at = current.and_then(|c| list.iter().position(|i| i.id == c));
    match dir {
        Dir::Next | Dir::Prev => {
            let n = list.len();
            let forward = dir == Dir::Next;
            let i = match (at, forward) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(i), true) if i + 1 < n => i + 1,
                (Some(i), false) if i > 0 => i - 1,
                _ if wrap => {
                    if forward {
                        0
                    } else {
                        n - 1
                    }
                }
                (Some(i), _) => i,
            };
            Some(list[i].id)
        }
        Dir::NextNeighbor | Dir::PrevNeighbor => {
            let at = at?;
            let here = list[at];
            let mut groups: Vec<Group> = Vec::new();
            for i in &list {
                if groups.last() != Some(&i.group) {
                    groups.push(i.group);
                }
            }
            let g = groups.iter().position(|g| *g == here.group)?;
            let forward = dir == Dir::NextNeighbor;
            let n = groups.len();
            let next = if forward {
                if g + 1 < n {
                    Some(g + 1)
                } else {
                    wrap.then_some(0)
                }
            } else if g > 0 {
                Some(g - 1)
            } else {
                wrap.then_some(n - 1)
            }?;
            let in_group: Vec<&&Item> = list.iter().filter(|i| i.group == groups[next]).collect();
            in_group
                .iter()
                .find(|i| i.role == here.role)
                .or_else(|| in_group.first())
                .map(|i| i.id)
        }
    }
}

/// Moves the keyboard focus. Returns whether a control has it now.
pub fn step_focus(ctx: &Context, area: FocusArea, dir: Dir, wrap: bool) -> bool {
    with(ctx, |d| {
        let items = if d.frame.is_empty() {
            &d.last
        } else {
            &d.frame
        };
        let next = next_id(items, d.current, area, dir, wrap);
        if next.is_some() {
            d.current = next;
            d.entry = None;
        }
        d.current.is_some()
    })
}

/// Gives the keyboard to the first control of `group`, as Move Focus to Mixer does.
pub fn focus_group(ctx: &Context, group: Group, area: FocusArea) -> bool {
    with(ctx, |d| {
        let items = if d.frame.is_empty() {
            &d.last
        } else {
            &d.frame
        };
        let found = items
            .iter()
            .find(|i| i.group == group && i.area == area)
            .map(|i| i.id);
        if found.is_some() {
            d.current = found;
            d.entry = None;
        }
        found.is_some()
    })
}

/// Gives the keyboard to a particular control of a group.
#[cfg(test)]
pub fn focus_role(ctx: &Context, group: Group, role: &str) -> bool {
    with(ctx, |d| {
        let items = if d.frame.is_empty() {
            &d.last
        } else {
            &d.frame
        };
        let found = items
            .iter()
            .find(|i| i.group == group && i.role == Id::unique(role))
            .map(|i| i.id);
        if found.is_some() {
            d.current = found;
        }
        found.is_some()
    })
}

/// Lets go of the keyboard.
pub fn clear(ctx: &Context) {
    with(ctx, |d| {
        d.current = None;
        d.entry = None;
    });
}

fn current_item(d: &Data) -> Option<&Item> {
    let id = d.current?;
    d.frame.iter().chain(d.last.iter()).find(|i| i.id == id)
}

/// The control with the keyboard, if it is still on screen.
pub fn focused(ctx: &Context) -> Option<Item> {
    with(ctx, |d| current_item(d).cloned())
}

/// A value is being typed.
pub fn entry(ctx: &Context) -> Option<String> {
    with(ctx, |d| d.entry.clone())
}

/// Sends the focused control an op, delivered when it draws this frame.
pub fn queue(ctx: &Context, op: ValueOp) {
    with(ctx, |d| {
        if let Some(id) = current_item(d).map(|i| i.id) {
            d.pending = Some((id, op));
        }
    });
}

/// Adds a character to the value being typed. Only value controls take typing.
pub fn type_char(ctx: &Context, c: char) {
    with(ctx, |d| {
        if current_item(d).is_some_and(|i| i.value) {
            d.entry.get_or_insert_with(String::new).push(c);
        }
    });
}

/// Enter: applies what was typed, or presses a button.
pub fn confirm(ctx: &Context) {
    with(ctx, |d| {
        let Some((id, value)) = current_item(d).map(|i| (i.id, i.value)) else {
            return;
        };
        if let Some(text) = d.entry.take() {
            if let Ok(v) = text.parse::<f64>() {
                d.pending = Some((id, ValueOp::Set(v)));
            }
        } else if !value {
            d.pending = Some((id, ValueOp::Press));
        }
    });
}

/// Esc: drops what was typed. Returns whether there was anything to drop.
pub fn cancel_entry(ctx: &Context) -> bool {
    with(ctx, |d| d.entry.take().is_some())
}

/// Which parsed value control types and text have to agree on, for fields like position
/// where `.` or `,` moves on to the next part: it is a decimal point everywhere else.
pub fn normalize_entry_char(c: char) -> char {
    if c == ',' {
        '.'
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect};

    fn item(n: u32, area: FocusArea, group: Group, role: &str) -> Item {
        Item {
            id: Id::unique(n),
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)),
            area,
            group,
            role: Id::unique(role),
            value: true,
        }
    }

    fn tid(n: u32) -> TrackId {
        TrackId(n.into())
    }

    fn mixer() -> Vec<Item> {
        let (a, b) = (Group::Track(tid(1)), Group::Track(tid(2)));
        vec![
            item(1, FocusArea::Session, a, "pan"),
            item(2, FocusArea::Session, a, "volume"),
            item(3, FocusArea::Session, b, "pan"),
            item(4, FocusArea::Session, b, "volume"),
            item(5, FocusArea::Device, Group::None, "knob"),
        ]
    }

    #[test]
    fn tab_follows_drawing_order_within_the_area() {
        let items = mixer();
        let s = FocusArea::Session;
        assert_eq!(
            next_id(&items, None, s, Dir::Next, false),
            Some(Id::unique(1u32))
        );
        assert_eq!(
            next_id(&items, Some(Id::unique(2u32)), s, Dir::Next, false),
            Some(Id::unique(3u32))
        );
        assert_eq!(
            next_id(&items, Some(Id::unique(3u32)), s, Dir::Prev, false),
            Some(Id::unique(2u32))
        );
    }

    #[test]
    fn tab_stops_at_the_ends_unless_wrapping() {
        let items = mixer();
        let s = FocusArea::Session;
        let last = Some(Id::unique(4u32));
        assert_eq!(next_id(&items, last, s, Dir::Next, false), last);
        assert_eq!(
            next_id(&items, last, s, Dir::Next, true),
            Some(Id::unique(1u32))
        );
        let first = Some(Id::unique(1u32));
        assert_eq!(next_id(&items, first, s, Dir::Prev, false), first);
        assert_eq!(
            next_id(&items, first, s, Dir::Prev, true),
            Some(Id::unique(4u32))
        );
    }

    #[test]
    fn neighbors_keep_the_same_control_on_the_next_track() {
        let items = mixer();
        let s = FocusArea::Session;
        assert_eq!(
            next_id(&items, Some(Id::unique(1u32)), s, Dir::NextNeighbor, false),
            Some(Id::unique(3u32))
        );
        assert_eq!(
            next_id(&items, Some(Id::unique(4u32)), s, Dir::PrevNeighbor, false),
            Some(Id::unique(2u32))
        );
        assert_eq!(
            next_id(&items, Some(Id::unique(3u32)), s, Dir::NextNeighbor, false),
            None,
            "there's no next track and wrapping is off"
        );
    }

    #[test]
    fn other_areas_are_not_in_the_order() {
        let items = mixer();
        assert_eq!(
            next_id(&items, None, FocusArea::Device, Dir::Next, false),
            Some(Id::unique(5u32))
        );
    }

    #[test]
    fn arrow_steps_and_defaults_compute_the_new_value() {
        assert_eq!(
            stepped(
                Some(ValueOp::Step {
                    up: true,
                    fine: false
                }),
                1.0,
                0.5,
                0.0
            ),
            Some(1.5)
        );
        assert_eq!(
            stepped(
                Some(ValueOp::Step {
                    up: false,
                    fine: true
                }),
                1.0,
                0.5,
                0.0
            ),
            Some(0.95)
        );
        assert_eq!(stepped(Some(ValueOp::Default), 1.0, 0.5, 0.25), Some(0.25));
        assert_eq!(stepped(Some(ValueOp::Set(3.0)), 1.0, 0.5, 0.0), Some(3.0));
        assert_eq!(stepped(Some(ValueOp::Press), 1.0, 0.5, 0.0), None);
        assert_eq!(stepped(None, 1.0, 0.5, 0.0), None);
    }

    #[test]
    fn typed_entry_commits_only_on_confirm_and_vanishes_on_cancel() {
        let ctx = Context::default();
        let id = Id::unique(7u32);
        with(&ctx, |d| {
            d.current = Some(id);
            d.last.push(Item {
                id,
                rect: Rect::ZERO,
                area: FocusArea::Session,
                group: Group::None,
                role: Id::unique("x"),
                value: true,
            });
        });
        type_char(&ctx, '1');
        type_char(&ctx, '.');
        type_char(&ctx, '5');
        assert_eq!(entry(&ctx).as_deref(), Some("1.5"));
        confirm(&ctx);
        with(&ctx, |d| {
            assert_eq!(d.pending, Some((id, ValueOp::Set(1.5))));
            d.pending = None;
        });
        type_char(&ctx, '9');
        assert!(cancel_entry(&ctx));
        assert_eq!(entry(&ctx), None);
        assert!(!cancel_entry(&ctx));
    }

    #[test]
    fn buttons_take_a_press_rather_than_typing() {
        let ctx = Context::default();
        let id = Id::unique(9u32);
        with(&ctx, |d| {
            d.current = Some(id);
            d.last.push(Item {
                id,
                rect: Rect::ZERO,
                area: FocusArea::Session,
                group: Group::None,
                role: Id::unique("arm"),
                value: false,
            });
        });
        type_char(&ctx, '5');
        assert_eq!(entry(&ctx), None);
        confirm(&ctx);
        with(&ctx, |d| {
            assert_eq!(d.pending, Some((id, ValueOp::Press)));
        });
        assert!(toggled(Some(ValueOp::Press), true));
        assert!(toggled(
            Some(ValueOp::Step {
                up: true,
                fine: false
            }),
            false
        ));
        assert!(!toggled(
            Some(ValueOp::Step {
                up: true,
                fine: false
            }),
            true
        ));
    }
}
