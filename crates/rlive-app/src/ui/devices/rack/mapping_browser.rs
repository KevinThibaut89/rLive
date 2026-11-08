//! The Mapping Browser (manual p. 505): every macro's mappings, with Min and Max.

use super::{MapMode, RackAction};
use crate::session::{ChainRef, Session};
use crate::ui::devices::{Action, ActionQueue};
use crate::ui::theme::{self, pal};
use crate::ui::widgets::info;
use crate::ui::UiState;
use egui::{RichText, Vec2};
use rlive_core::{ParamInfo, ParamOwner, ParamRef};
use rlive_project::{Device, DeviceKind};

/// Text of a value in a parameter's own units.
pub fn format_value(session: &Session, info: &ParamInfo, target: ParamRef, value: f64) -> String {
    let device = match target.owner {
        ParamOwner::Device(id) => session.runtimes.get(&id),
        _ => None,
    };
    match device {
        Some(rt) => rt.format_value(info, value),
        None => format!("{value:.1} {}", info.unit),
    }
}

/// What a mapping's target is called: `Device: Parameter`.
pub fn target_name(rack: &Device, target: ParamRef, info: &ParamInfo) -> String {
    let owner = match target.owner {
        ParamOwner::Device(id) => {
            let mut name = None;
            rack.for_each(&mut |d| {
                if d.id == id {
                    name = Some(d.name.clone());
                }
            });
            name
        }
        ParamOwner::Chain(id) => {
            let mut name = None;
            rack.for_each(&mut |d| {
                if let Some(c) = d.chains().find(|c| c.id == id) {
                    name = Some(c.name.clone());
                }
            });
            name
        }
        _ => None,
    };
    match owner {
        Some(owner) => format!("{owner}: {}", info.name),
        None => info.name.clone(),
    }
}

/// Draws the browser while Map Mode is on.
pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut UiState) {
    let Some(MapMode { chain, rack, .. }) = state.devices.ui.map else {
        return;
    };
    let mut queue = ActionQueue::default();
    {
        let session: &Session = session;
        ui.label(
            RichText::new("Mapping Browser")
                .font(theme::font_bold(12.0))
                .color(pal().text),
        );
        ui.separator();
        match session
            .devices(chain)
            .iter()
            .find(|d| d.id == rack)
            .filter(|d| matches!(d.kind, DeviceKind::Rack { .. }))
        {
            Some(device) => mappings(ui, session, chain, device, &mut queue),
            None => {
                ui.label(RichText::new("This Rack is gone").color(pal().text_dim));
            }
        }
    }
    queue.apply(session, &mut state.devices, &mut state.status);
}

fn mappings(
    ui: &mut egui::Ui,
    session: &Session,
    chain: ChainRef,
    rack: &Device,
    queue: &mut ActionQueue,
) {
    let DeviceKind::Rack { macros, .. } = &rack.kind else {
        return;
    };
    let mut any = false;
    egui::ScrollArea::vertical()
        .id_salt("mapping_browser")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (n, m) in macros.iter().enumerate() {
                for (i, map) in m.mappings.iter().enumerate() {
                    let Some(info_) = session.param_info(chain, map.target) else {
                        continue;
                    };
                    any = true;
                    row(ui, session, chain, rack, n, i, *map, &info_, queue);
                }
            }
            if !any {
                ui.label(
                    RichText::new(
                        "Click a parameter, then a macro's Map button.\nMappings appear here.",
                    )
                    .color(pal().text_dim)
                    .size(11.0),
                );
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut egui::Ui,
    session: &Session,
    chain: ChainRef,
    rack: &Device,
    n: usize,
    i: usize,
    map: rlive_project::MacroMapping,
    param: &ParamInfo,
    queue: &mut ActionQueue,
) {
    let id = rack.id;
    let name = target_name(rack, map.target, param);
    let group = ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            RichText::new(format!("Macro {}", n + 1))
                .color(pal().accent)
                .size(10.0),
        );
        ui.label(RichText::new(name).size(11.0).color(pal().text));
        let (lo, hi) = (param.min, param.max);
        for (label, value, is_min) in [("Min", map.min, true), ("Max", map.max, false)] {
            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size = Vec2::new(70.0, 14.0);
                ui.label(RichText::new(label).size(10.0).color(pal().text_dim));
                let mut v = value;
                let changed = ui
                    .add(egui::Slider::new(&mut v, lo..=hi).show_value(false))
                    .changed();
                ui.label(
                    RichText::new(format_value(session, param, map.target, value))
                        .size(10.0)
                        .color(pal().text),
                );
                if changed {
                    let (min, max) = if is_min { (v, map.max) } else { (map.min, v) };
                    queue.push(chain, Action::Rack(id, RackAction::Range(n, i, min, max)));
                }
            });
        }
    });
    let resp = group
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::Default);
    let resp = info(resp, "Mapping — right-click to invert or delete it");
    resp.context_menu(|ui| {
        if ui.button("Invert Range").clicked() {
            queue.push(chain, Action::Rack(id, RackAction::Invert(n, i)));
            ui.close();
        }
        if ui
            .button(RichText::new("Delete").color(pal().arm_red))
            .clicked()
        {
            queue.push(chain, Action::Rack(id, RackAction::Unmap(n, i)));
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::ParamId;
    use rlive_project::{Chain, RackKind};

    #[test]
    fn a_mapping_names_its_device_and_parameter() {
        let mut rack = Device::rack(RackKind::Audio, "Rack");
        let mut c = Chain::new("A");
        let comp = Device::builtin("rlive.compressor", "Compressor");
        let comp_id = comp.id;
        let cid = c.id;
        c.devices.push(comp);
        if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
            chains.push(c);
        }
        let info_ = ParamInfo::continuous(0, "Threshold", -60.0, 0.0, 0.0, "dB");
        let device_target = ParamRef {
            owner: ParamOwner::Device(comp_id),
            id: ParamId(0),
        };
        assert_eq!(
            target_name(&rack, device_target, &info_),
            "Compressor: Threshold"
        );
        let chain_target = ParamRef {
            owner: ParamOwner::Chain(cid),
            id: ParamId(0),
        };
        assert_eq!(target_name(&rack, chain_target, &info_), "A: Threshold");
    }
}
