//! The Drum Rack's Pad View (manual pp. 502-503): 16 of the 128 notes at a time.

use super::super::DrawCx;
use super::{RackAction, RackRef};
use crate::session::ChainRef;
use crate::ui::devices::Action;
use crate::ui::theme::{self, pal, shade};
use crate::ui::widgets::{info, rgb};
use crate::ui::BrowserItem;
use egui::{Align2, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use rlive_core::param::chain as controls;
use rlive_core::{ChainId, DeviceId};
use rlive_project::{Chain, Device, DeviceKind, RackKind};

/// Pads on a page.
pub const PAD_PAGE: u8 = 16;
/// Pads in a row.
pub const PAD_ROW: u8 = 4;
/// The lowest pad the top of the scroll can show.
const LAST_PAGE: u8 = 128 - PAD_PAGE;
/// The width of the pad overview.
pub const OVERVIEW_W: f32 = 16.0;

/// Live calls middle C "C3".
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[(note % 12) as usize], (note / 12) as i32 - 2)
}

/// The General MIDI drum on a note, where there is one.
pub fn gm_drum_name(note: u8) -> Option<&'static str> {
    const NAMES: [&str; 47] = [
        "Acoustic Bass Drum",
        "Bass Drum 1",
        "Side Stick",
        "Acoustic Snare",
        "Hand Clap",
        "Electric Snare",
        "Low Floor Tom",
        "Closed Hi-Hat",
        "High Floor Tom",
        "Pedal Hi-Hat",
        "Low Tom",
        "Open Hi-Hat",
        "Low-Mid Tom",
        "Hi-Mid Tom",
        "Crash Cymbal 1",
        "High Tom",
        "Ride Cymbal 1",
        "Chinese Cymbal",
        "Ride Bell",
        "Tambourine",
        "Splash Cymbal",
        "Cowbell",
        "Crash Cymbal 2",
        "Vibraslap",
        "Ride Cymbal 2",
        "Hi Bongo",
        "Low Bongo",
        "Mute Hi Conga",
        "Open Hi Conga",
        "Low Conga",
        "High Timbale",
        "Low Timbale",
        "High Agogo",
        "Low Agogo",
        "Cabasa",
        "Maracas",
        "Short Whistle",
        "Long Whistle",
        "Short Guiro",
        "Long Guiro",
        "Claves",
        "Hi Wood Block",
        "Low Wood Block",
        "Mute Cuica",
        "Open Cuica",
        "Mute Triangle",
        "Open Triangle",
    ];
    NAMES.get((note as usize).checked_sub(35)?).copied()
}

/// The note on a pad: the page is bottom-left to top-right, `row` counted from the top.
pub fn pad_note(start: u8, col: u8, row: u8) -> Option<u8> {
    if col >= PAD_ROW || row >= PAD_PAGE / PAD_ROW {
        return None;
    }
    let row_from_bottom = PAD_PAGE / PAD_ROW - 1 - row;
    start.checked_add(row_from_bottom * PAD_ROW + col)
}

/// Scrolls by whole pages, or by rows with the modifier held. Stays in `0..=112`.
pub fn scroll_pads(start: u8, up: bool, by_row: bool) -> u8 {
    let step = if by_row { PAD_ROW } else { PAD_PAGE } as i32;
    let next = start as i32 + if up { step } else { -step };
    next.clamp(0, LAST_PAGE as i32) as u8
}

/// The first note of the page whose rows are centred on `y` in the overview, a fraction of its
/// height with the top at zero. The overview shows note 127 at the top.
pub fn overview_start(y: f32) -> u8 {
    let rows = 128 / PAD_ROW as i32;
    let row_from_bottom = ((1.0 - y.clamp(0.0, 1.0)) * rows as f32) as i32;
    ((row_from_bottom - 1).max(0) * PAD_ROW as i32).min(LAST_PAGE as i32) as u8
}

/// Whether a modifier state means "layer" on a drag: Cmd on the Mac, Alt elsewhere (p. 503).
pub fn layers(m: egui::Modifiers, platform: crate::ui::keymap::Platform) -> bool {
    match platform {
        crate::ui::keymap::Platform::Mac => m.mac_cmd || m.command,
        crate::ui::keymap::Platform::Other => m.alt,
    }
}

/// A chain a pad triggers.
#[derive(Debug, Clone, PartialEq)]
pub struct PadTarget {
    /// The device chain holding the rack the chain belongs to.
    pub at: ChainRef,
    pub rack: DeviceId,
    pub chain: ChainId,
    pub name: String,
    pub color: Option<[u8; 3]>,
    pub active: bool,
    pub solo: bool,
}

/// What a pad stands for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PadInfo {
    pub targets: Vec<PadTarget>,
}

/// Whether a state is on for none, some or all of a pad's chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mixed {
    None,
    Some,
    All,
}

impl PadInfo {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn is_multi(&self) -> bool {
        self.targets.len() > 1
    }

    /// The pad's name: its chain's, "Multi" for several, nothing for an empty pad (p. 503).
    pub fn label(&self) -> Option<&str> {
        match self.targets.as_slice() {
            [] => None,
            [one] => Some(&one.name),
            _ => Some("Multi"),
        }
    }

    pub fn mute_state(&self) -> Mixed {
        mixed(self.targets.iter().map(|t| !t.active))
    }

    pub fn solo_state(&self) -> Mixed {
        mixed(self.targets.iter().map(|t| t.solo))
    }
}

fn mixed(states: impl Iterator<Item = bool>) -> Mixed {
    let (mut on, mut off) = (false, false);
    for s in states {
        if s {
            on = true;
        } else {
            off = true;
        }
    }
    match (on, off) {
        (true, false) => Mixed::All,
        (true, true) => Mixed::Some,
        _ => Mixed::None,
    }
}

/// All the chains that answer `note` under this Drum Rack, however deep.
pub fn pad_info(at: ChainRef, rack: &Device, note: u8) -> PadInfo {
    let mut targets = Vec::new();
    if let DeviceKind::Rack { chains, .. } = &rack.kind {
        for c in chains
            .iter()
            .filter(|c| c.zones.drum.is_some_and(|p| p.receive == Some(note)))
        {
            let play = c.zones.drum.map_or(note, |p| p.play);
            collect_targets(at, rack.id, c, play, &mut targets);
        }
    }
    PadInfo { targets }
}

/// A chain counts as its answering nested chains if it holds a rack, or as itself if it doesn't.
fn collect_targets(
    at: ChainRef,
    rack: DeviceId,
    chain: &Chain,
    note: u8,
    out: &mut Vec<PadTarget>,
) {
    let before = out.len();
    if let Some(child) = at.child(rack, chain.id) {
        for d in &chain.devices {
            let DeviceKind::Rack {
                kind: RackKind::Instrument | RackKind::Drum,
                chains,
                ..
            } = &d.kind
            else {
                continue;
            };
            for inner in chains.iter().filter(|c| chain_hears(c, note)) {
                let play = pad_play(inner, note);
                collect_targets(child, d.id, inner, play, out);
            }
        }
    }
    if out.len() == before {
        out.push(PadTarget {
            at,
            rack,
            chain: chain.id,
            name: chain.name.clone(),
            color: chain.color,
            active: chain.mix.active,
            solo: chain.mix.solo,
        });
    }
}

fn chain_hears(chain: &Chain, note: u8) -> bool {
    match chain.zones.drum {
        Some(pad) => pad.receive.is_none_or(|r| r == note),
        None => (chain.zones.key.fade_low..=chain.zones.key.fade_high).contains(&note),
    }
}

fn pad_play(chain: &Chain, note: u8) -> u8 {
    chain
        .zones
        .drum
        .filter(|p| p.receive.is_some())
        .map_or(note, |p| p.play)
}

/// A pad being dragged, to swap it with another or layer them.
#[derive(Clone, Copy, Debug)]
pub struct PadDrag {
    pub rack: DeviceId,
    pub note: u8,
}

/// Draws the page and its overview over `rect`.
pub fn view(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, rect: Rect) {
    let overview = Rect::from_min_size(rect.min, Vec2::new(OVERVIEW_W, rect.height()));
    let grid = Rect::from_min_max(Pos2::new(overview.right() + 3.0, rect.top()), rect.max);
    let start = r.view.pad_start;
    overview_strip(ui, cx, r, overview);
    let cols = PAD_ROW as f32;
    let rows = (PAD_PAGE / PAD_ROW) as f32;
    let pad_size = Vec2::new(grid.width() / cols, grid.height() / rows);
    let mut pads = Vec::with_capacity(PAD_PAGE as usize);
    for row in 0..PAD_PAGE / PAD_ROW {
        for col in 0..PAD_ROW {
            let Some(note) = pad_note(start, col, row) else {
                continue;
            };
            let min = grid.min + Vec2::new(col as f32 * pad_size.x, row as f32 * pad_size.y);
            let pad_rect = Rect::from_min_size(min, pad_size).shrink(1.0);
            pads.push((note, pad_rect));
        }
    }
    for (note, pad_rect) in pads {
        let mut pu = ui.new_child(egui::UiBuilder::new().max_rect(pad_rect));
        draw_pad(&mut pu, cx, r, note, pad_rect);
    }
    let wants = rect.contains(ui.ctx().pointer_latest_pos().unwrap_or_default())
        || cx.ui.key_focus == Some(super::KeyFocus::Pads(r.dev.id));
    if wants && cx.keyboard_free {
        let (up, down, by_row) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.modifiers.alt || i.modifiers.command,
            )
        });
        if up || down {
            let next = scroll_pads(start, up, by_row);
            if next != start {
                cx.queue
                    .push(r.chain, Action::Rack(r.dev.id, RackAction::PadStart(next)));
            }
        }
    }
}

fn overview_strip(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, rect: Rect) {
    let p = ui.painter().clone();
    p.rect_filled(rect, 1.0, pal().field_bg);
    let rows = 128 / PAD_ROW as usize;
    let cell = Vec2::new(rect.width() / PAD_ROW as f32, rect.height() / rows as f32);
    let start = r.view.pad_start as usize;
    for note in 0..128usize {
        let col = (note % PAD_ROW as usize) as f32;
        let row = rows - 1 - note / PAD_ROW as usize;
        let min = rect.min + Vec2::new(col * cell.x, row as f32 * cell.y);
        let on_page = note >= start && note < start + PAD_PAGE as usize;
        let used = r
            .chains
            .iter()
            .any(|c| c.zones.drum.is_some_and(|d| d.receive == Some(note as u8)));
        let c = if used {
            pal().accent
        } else if on_page {
            pal().raised_hover
        } else {
            pal().raised
        };
        p.rect_filled(
            Rect::from_min_size(min, cell).shrink(0.5),
            0.0,
            if used && !on_page {
                theme::darken(c, 0.4)
            } else {
                c
            },
        );
    }
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("pad_overview", r.dev.id)),
        Sense::click_and_drag(),
    );
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let next = overview_start((pos.y - rect.top()) / rect.height());
            if next != r.view.pad_start {
                cx.queue
                    .push(r.chain, Action::Rack(r.dev.id, RackAction::PadStart(next)));
            }
        }
    }
    info(resp, "Pad Overview — drag to show other pads");
}

fn draw_pad(ui: &mut egui::Ui, cx: &mut DrawCx, r: &RackRef, note: u8, rect: Rect) {
    let info_ = pad_info(r.chain, r.dev, note);
    let selected = r.view.last_pad == Some(note);
    let p = ui.painter().clone();
    let color = info_
        .targets
        .first()
        .and_then(|t| t.color)
        .map_or(pal().accent, rgb);
    let bg = if info_.is_empty() {
        shade(0x2a2a2a)
    } else {
        theme::darken(color, 0.55)
    };
    p.rect_filled(rect, 2.0, bg);
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("pad", r.dev.id, note)),
        Sense::click_and_drag(),
    );
    let drop_target = resp.dnd_hover_payload::<BrowserItem>().is_some()
        || resp.dnd_hover_payload::<PadDrag>().is_some();
    if selected || drop_target {
        p.rect_stroke(
            rect,
            2.0,
            Stroke::new(
                1.0,
                if drop_target {
                    pal().accent
                } else {
                    shade(0xdddddd)
                },
            ),
            StrokeKind::Inside,
        );
    }
    let name = info_.label().unwrap_or("");
    let text = theme::text_on(bg);
    let label = if name.is_empty() {
        note_name(note)
    } else {
        name.to_string()
    };
    p.with_clip_rect(rect.shrink(2.0)).text(
        rect.left_top() + Vec2::new(3.0, 2.0),
        Align2::LEFT_TOP,
        label,
        theme::font(9.5),
        if name.is_empty() {
            pal().text_faint
        } else {
            text
        },
    );
    if !info_.is_empty() {
        pad_buttons(ui, cx, r, note, &info_, rect);
    }
    let rename_target = match info_.targets.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    };
    let targets = info_.targets.clone();
    let multi = info_.is_multi();
    let hover = pad_hover_text(note, &info_);
    let resp = info(resp, hover);
    if resp.clicked() {
        cx.ui.key_focus = Some(super::KeyFocus::Pads(r.dev.id));
        let chain = info_.targets.first().filter(|t| t.rack == r.dev.id);
        cx.queue.push(
            r.chain,
            Action::Rack(
                r.dev.id,
                RackAction::SelectPad(note, chain.map(|t| t.chain)),
            ),
        );
    }
    if resp.drag_started() && !info_.is_empty() {
        resp.dnd_set_drag_payload(PadDrag {
            rack: r.dev.id,
            note,
        });
    }
    let platform = crate::ui::keymap::Platform::current();
    let layer = layers(ui.input(|i| i.modifiers), platform);
    if let Some(item) = crate::ui::widgets::released::<BrowserItem>(&resp) {
        let action = match &*item {
            BrowserItem::Many(items) => RackAction::PadDropMany(note, items.clone(), layer),
            one => RackAction::PadDrop(note, one.clone()),
        };
        cx.queue.push(r.chain, Action::Rack(r.dev.id, action));
    }
    if let Some(drag) = crate::ui::widgets::released::<PadDrag>(&resp) {
        if drag.rack == r.dev.id && drag.note != note {
            cx.queue.push(
                r.chain,
                Action::Rack(
                    r.dev.id,
                    if layer {
                        RackAction::PadLayer(drag.note, note)
                    } else {
                        RackAction::PadSwap(drag.note, note)
                    },
                ),
            );
        }
    }
    resp.context_menu(|ui| {
        if ui.button("Hot-Swap").clicked() {
            cx.queue.push(
                r.chain,
                Action::HotSwap(crate::session::SwapTarget::Pad {
                    chain: r.chain,
                    rack: r.dev.id,
                    note,
                }),
            );
            ui.close();
        }
        if !info_.is_empty() && ui.button("Save as Default Pad").clicked() {
            cx.queue
                .push(r.chain, Action::SaveDefaultPad(r.dev.id, note));
            ui.close();
        }
        if let Some(target) = &rename_target {
            if ui.button("Rename").clicked() {
                cx.ui.rename = Some(super::Rename {
                    target: super::RenameTarget::Chain(target.rack, target.chain),
                    text: target.name.clone(),
                });
                ui.close();
            }
        }
        let label = if multi { "Delete All Chains" } else { "Delete" };
        if !targets.is_empty()
            && ui
                .button(egui::RichText::new(label).color(pal().arm_red))
                .clicked()
        {
            for t in &targets {
                cx.queue.push(
                    t.at,
                    Action::Rack(t.rack, RackAction::DeleteChains(vec![t.chain])),
                );
            }
            ui.close();
        }
    });
}

fn pad_hover_text(note: u8, info_: &PadInfo) -> String {
    let drum = gm_drum_name(note).map_or(String::new(), |g| format!(" ({g})"));
    match info_.label() {
        None => format!("Pad {}{drum} — drop anything here", note_name(note)),
        Some(name) => format!("Pad {} — {name}", note_name(note)),
    }
}

fn pad_buttons(
    ui: &mut egui::Ui,
    cx: &mut DrawCx,
    r: &RackRef,
    note: u8,
    info_: &PadInfo,
    rect: Rect,
) {
    let size = Vec2::new(14.0, 12.0);
    let y = rect.bottom() - size.y - 2.0;
    let m = Rect::from_min_size(Pos2::new(rect.left() + 2.0, y), size);
    let s = Rect::from_min_size(Pos2::new(m.right() + 1.0, y), size);
    let pv = Rect::from_min_size(Pos2::new(s.right() + 1.0, y), size);
    let mute = info_.mute_state();
    let solo = info_.solo_state();
    let (mute_r, solo_r) = (
        small_button(ui, m, ("pad_m", r.dev.id, note), "M", mute, pal().activator),
        small_button(ui, s, ("pad_s", r.dev.id, note), "S", solo, pal().solo_blue),
    );
    if info(mute_r, "Mute this pad's chains").clicked() {
        // Mutes when nothing is muted, and otherwise brings them all back.
        let active = mute == Mixed::None;
        for t in &info_.targets {
            cx.queue.push(
                t.at,
                Action::Rack(
                    t.rack,
                    RackAction::ChainParam(t.chain, controls::ACTIVATOR, (!active) as u8 as f64),
                ),
            );
        }
    }
    if info(solo_r, "Solo this pad's chains").clicked() {
        let solo_on = solo != Mixed::All;
        for t in &info_.targets {
            cx.queue.push(
                t.at,
                Action::Rack(
                    t.rack,
                    RackAction::ChainParam(t.chain, controls::SOLO, solo_on as u8 as f64),
                ),
            );
        }
    }
    let pv_r = ui.interact(
        pv,
        ui.scope_id().with(("pad_pv", r.dev.id, note)),
        Sense::click(),
    );
    ui.painter().rect_filled(pv, 1.0, pal().raised);
    crate::ui::icons::play(ui.painter(), pv.shrink(2.0), pal().text);
    let pressed = pv_r.is_pointer_button_down_on();
    info(pv_r, "Preview — fires this pad's note");
    if pressed && cx.ui.preview.is_none() {
        let active = super::ActivePreview {
            chain: r.chain,
            rack: r.dev.id,
            target: None,
            key: note,
        };
        cx.ui.preview = Some(active);
        cx.queue.push(
            r.chain,
            Action::Rack(r.dev.id, RackAction::Preview(active, true)),
        );
    }
}

fn small_button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: impl egui::AsIdSalt,
    label: &str,
    state: Mixed,
    on: egui::Color32,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let p = ui.painter();
    let fill = match state {
        Mixed::All => on,
        Mixed::Some => theme::darken(on, 0.5),
        Mixed::None => pal().raised,
    };
    p.rect_filled(rect, 1.0, fill);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::font(9.0),
        theme::text_on(fill),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::TrackId;
    use rlive_project::DrumPad;

    #[test]
    fn notes_read_as_live_names_them() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(0), "C-2");
        assert_eq!(note_name(127), "G8");
        assert_eq!(note_name(36), "C1");
    }

    #[test]
    fn the_gm_names_cover_their_range_only() {
        assert_eq!(gm_drum_name(36), Some("Bass Drum 1"));
        assert_eq!(gm_drum_name(38), Some("Acoustic Snare"));
        assert_eq!(gm_drum_name(42), Some("Closed Hi-Hat"));
        assert_eq!(gm_drum_name(34), None);
        assert_eq!(gm_drum_name(82), None);
    }

    #[test]
    fn a_page_runs_up_from_the_bottom_left() {
        assert_eq!(pad_note(36, 0, 3), Some(36));
        assert_eq!(pad_note(36, 3, 3), Some(39));
        assert_eq!(pad_note(36, 0, 0), Some(48));
        assert_eq!(pad_note(36, 3, 0), Some(51));
        assert_eq!(pad_note(36, 4, 0), None);
    }

    #[test]
    fn the_top_page_reaches_note_127_exactly() {
        assert_eq!(pad_note(112, 3, 0), Some(127));
        assert_eq!(pad_note(0, 0, 3), Some(0));
    }

    #[test]
    fn scrolling_goes_by_page_or_row_and_stops_at_the_ends() {
        assert_eq!(scroll_pads(36, true, false), 52);
        assert_eq!(scroll_pads(36, false, false), 20);
        assert_eq!(scroll_pads(36, true, true), 40);
        assert_eq!(scroll_pads(36, false, true), 32);
        assert_eq!(scroll_pads(4, false, false), 0);
        assert_eq!(scroll_pads(108, true, false), 112);
    }

    #[test]
    fn the_overview_covers_every_page() {
        assert_eq!(overview_start(1.0), 0);
        assert_eq!(overview_start(0.0), 112);
        let mid = overview_start(0.5);
        assert_eq!(mid % 4, 0);
        assert!((48..=64).contains(&mid), "{mid}");
    }

    #[test]
    fn the_layer_modifier_is_cmd_on_the_mac_and_alt_elsewhere() {
        use crate::ui::keymap::Platform;
        let cmd = egui::Modifiers::MAC_CMD;
        assert!(layers(cmd, Platform::Mac));
        assert!(!layers(cmd, Platform::Other));
        assert!(layers(egui::Modifiers::ALT, Platform::Other));
        assert!(!layers(egui::Modifiers::ALT, Platform::Mac));
    }

    fn drum(chains: Vec<(&str, Option<u8>)>) -> Device {
        let mut d = Device::rack(RackKind::Drum, "Drums");
        if let DeviceKind::Rack { chains: c, .. } = &mut d.kind {
            *c = chains
                .into_iter()
                .map(|(name, receive)| {
                    let mut c = Chain::new(name);
                    c.zones.drum = Some(DrumPad {
                        receive,
                        play: 60,
                        choke: None,
                    });
                    c
                })
                .collect();
        }
        d
    }

    fn root() -> ChainRef {
        ChainRef::Track(TrackId(1))
    }

    #[test]
    fn an_empty_pad_has_no_name_and_a_shared_note_is_multi() {
        let d = drum(vec![
            ("Kick", Some(36)),
            ("Snare", Some(36)),
            ("Hat", Some(42)),
        ]);
        assert!(pad_info(root(), &d, 40).is_empty());
        assert_eq!(pad_info(root(), &d, 40).label(), None);
        assert_eq!(pad_info(root(), &d, 42).label(), Some("Hat"));
        let multi = pad_info(root(), &d, 36);
        assert!(multi.is_multi());
        assert_eq!(multi.label(), Some("Multi"));
    }

    #[test]
    fn a_multi_pad_reports_mixed_mute_and_solo() {
        let mut d = drum(vec![("Kick", Some(36)), ("Snare", Some(36))]);
        if let DeviceKind::Rack { chains, .. } = &mut d.kind {
            chains[0].mix.active = false;
            chains[1].mix.solo = true;
        }
        let i = pad_info(root(), &d, 36);
        assert_eq!(i.mute_state(), Mixed::Some);
        assert_eq!(i.solo_state(), Mixed::Some);
        let one = pad_info(root(), &drum(vec![("Kick", Some(36))]), 36);
        assert_eq!(one.mute_state(), Mixed::None);
        assert_eq!(one.solo_state(), Mixed::None);
    }

    #[test]
    fn all_notes_chains_are_not_on_any_pad() {
        let d = drum(vec![("All", None)]);
        assert!(pad_info(root(), &d, 60).is_empty());
    }

    #[test]
    fn a_pad_looks_through_a_nested_rack_to_the_chains_that_answer() {
        let mut d = drum(vec![("Layers", Some(36))]);
        let mut inner = Device::rack(RackKind::Instrument, "Inner");
        if let DeviceKind::Rack { chains, .. } = &mut inner.kind {
            chains.push(Chain::new("A"));
            chains.push(Chain::new("B"));
            let mut out = Chain::new("Out of range");
            out.zones.key = rlive_project::Zone::span(0, 10);
            chains.push(out);
        }
        if let DeviceKind::Rack { chains, .. } = &mut d.kind {
            chains[0].devices.push(inner);
        }
        let i = pad_info(root(), &d, 36);
        let names: Vec<&str> = i.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A", "B"]);
        assert!(i.is_multi());
    }
}
