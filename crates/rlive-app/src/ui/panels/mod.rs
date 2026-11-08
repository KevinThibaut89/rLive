//! Custom device panels, drawn for [`Item::Panel`](rlive_core::Item::Panel) items.
//!
//! To add one: write a file here with a [`PanelDef`] named `PANEL`, add a row to [`PANELS`], and
//! put `Item::Panel("its name")` in the device's layout. The renderer finds it by name.

mod auto_filter_display;
mod b1_leds;
mod channel_eq_display;
mod clip_led;
mod compressor_display;
mod echo_tunnel;
mod eq_eight_display;
mod eq_three_leds;
mod gain_reduction;
mod gate_display;
mod looper_transport;
mod midi_monitor;
mod multiband_display;
mod plot;
mod roar_matrix;
mod scale_matrix;
mod velocity_curve;

use super::devices::{Action, ActionQueue};
use crate::session::{ChainRef, DeviceRuntime};
use egui::{Align2, Pos2, Stroke, StrokeKind, Vec2};
use rlive_core::{Item, Section, METER_VALUES, SCOPE_VALUES};
use rlive_core::{ParamId, ParamInfo, ViewReader};
use rlive_project::Device;

/// What a custom panel sees and can do.
pub struct PanelCtx<'a> {
    pub device: &'a Device,
    runtime: &'a DeviceRuntime,
    chain: ChainRef,
    /// The device's view channel, or `None` while it's bypassed or it has none.
    pub view: Option<&'a ViewReader>,
    queue: &'a mut ActionQueue,
    /// How long the last frame took, in seconds.
    pub dt: f32,
}

// The Compressor's panel only reads a view. These are what a panel that shows or changes
// parameters needs, so it can be written without touching the renderer.
#[allow(dead_code)]
impl<'a> PanelCtx<'a> {
    pub(super) fn new(
        chain: ChainRef,
        device: &'a Device,
        runtime: &'a DeviceRuntime,
        view: Option<&'a ViewReader>,
        queue: &'a mut ActionQueue,
        dt: f32,
    ) -> Self {
        Self {
            device,
            runtime,
            chain,
            view,
            queue,
            dt,
        }
    }

    /// A parameter of this device, unless it's hidden or gone.
    pub fn param(&self, id: ParamId) -> Option<&'a ParamInfo> {
        self.runtime.params.iter().find(|p| p.param_id() == id)
    }

    /// The value a parameter is set to.
    pub fn value(&self, id: ParamId) -> Option<f64> {
        let param = self.param(id)?;
        Some(
            self.device
                .params
                .get(&param.id)
                .copied()
                .unwrap_or(param.default),
        )
    }

    /// Change a parameter, once the frame has been drawn.
    pub fn set(&mut self, id: ParamId, value: f64) {
        self.queue
            .push(self.chain, Action::Param(self.device.id, id.0, value));
    }
}

/// A custom panel: one row of the table, usually defined next to its drawing code.
pub struct PanelDef {
    /// The name in `Item::Panel`.
    pub name: &'static str,
    /// The space a panel takes in a device's layout.
    pub size: Vec2,
    /// How many view values the panel reads. The channel is made big enough to hold them.
    pub views: usize,
    pub draw: fn(&mut egui::Ui, &mut PanelCtx),
}

/// Every custom panel. One row per panel.
pub const PANELS: &[PanelDef] = &[
    gain_reduction::PANEL,
    compressor_display::PANEL,
    gate_display::PANEL,
    multiband_display::PANEL,
    clip_led::PANEL,
    b1_leds::ARP,
    b1_leds::CHORD,
    b1_leds::PITCH,
    b1_leds::RANDOM,
    b1_leds::VELOCITY,
    midi_monitor::PANEL,
    scale_matrix::PANEL,
    velocity_curve::PANEL,
    eq_eight_display::PANEL,
    eq_three_leds::PANEL,
    channel_eq_display::PANEL,
    auto_filter_display::PANEL,
    roar_matrix::PANEL,
    echo_tunnel::PANEL,
    looper_transport::PANEL,
];

/// What a panel takes up when the table has no entry for it.
const MISSING_SIZE: Vec2 = Vec2::new(90.0, 40.0);

fn find(name: &str) -> Option<&'static PanelDef> {
    PANELS.iter().find(|p| p.name == name)
}

/// The size to reserve for a panel.
pub fn size(name: &str) -> Vec2 {
    find(name).map_or(MISSING_SIZE, |p| p.size)
}

/// How many values long a device's view channel must be for what its layout shows. Zero means
/// nothing it shows reads one, so it doesn't need a channel at all.
pub fn view_len(layout: &[Section]) -> usize {
    fn of(item: &Item) -> usize {
        match item {
            Item::Knob(_) | Item::Slider(_) | Item::Toggle(_) | Item::Menu(_) => 0,
            Item::Meter(view) => view.0 as usize + METER_VALUES,
            Item::Scope(view, _) => view.0 as usize + SCOPE_VALUES,
            Item::Row(items) => items.iter().map(of).max().unwrap_or(0),
            Item::Panel(name) => find(name).map_or(0, |p| p.views),
        }
    }
    layout
        .iter()
        .flat_map(|s| s.items.iter())
        .map(of)
        .max()
        .unwrap_or(0)
}

/// Draw a panel, or a placeholder if the table doesn't have it.
pub fn draw(name: &str, ui: &mut egui::Ui, panel: &mut PanelCtx) {
    let Some(def) = find(name) else {
        let rect = ui.max_rect();
        ui.painter().rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0, super::theme::pal().arm_red),
            StrokeKind::Inside,
        );
        ui.painter().text(
            Pos2::new(rect.center().x, rect.center().y),
            Align2::CENTER_CENTER,
            "Missing panel",
            super::theme::font(10.0),
            super::theme::pal().text_dim,
        );
        return;
    };
    (def.draw)(ui, panel);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::{ParamId, ScopeKind, ViewId};

    fn section(items: Vec<Item>) -> Section {
        Section { title: "", items }
    }

    #[test]
    fn a_layout_of_controls_needs_no_channel() {
        assert_eq!(view_len(&[]), 0);
        assert_eq!(
            view_len(&[section(vec![
                Item::Knob(ParamId(0)),
                Item::Slider(ParamId(1)),
                Item::Row(vec![Item::Toggle(ParamId(2)), Item::Menu(ParamId(3))]),
            ])]),
            0
        );
    }

    #[test]
    fn a_meter_or_scope_needs_room_for_its_values() {
        assert_eq!(
            view_len(&[section(vec![Item::Meter(ViewId(3))])]),
            3 + METER_VALUES
        );
        assert_eq!(
            view_len(&[section(vec![Item::Scope(ViewId(1), ScopeKind::Waveform)])]),
            1 + SCOPE_VALUES
        );
    }

    #[test]
    fn the_longest_view_wins_even_inside_a_row() {
        let layout = [section(vec![
            Item::Meter(ViewId(0)),
            Item::Row(vec![Item::Meter(ViewId(10))]),
        ])];
        assert_eq!(view_len(&layout), 10 + METER_VALUES);
    }

    #[test]
    fn a_panel_declares_what_it_reads_in_the_table() {
        assert_eq!(
            view_len(&[section(vec![Item::Panel("gain_reduction")])]),
            gain_reduction::PANEL.views
        );
        assert_eq!(view_len(&[section(vec![Item::Panel("no_such_panel")])]), 0);
    }

    #[test]
    fn panel_names_are_unique_and_panels_have_room() {
        for (i, a) in PANELS.iter().enumerate() {
            assert!(a.size.x > 0.0 && a.size.y > 0.0, "{}", a.name);
            assert!(
                PANELS[i + 1..].iter().all(|b| b.name != a.name),
                "duplicate panel {}",
                a.name
            );
        }
    }

    #[test]
    fn a_panel_reads_and_changes_parameters_of_its_own_device() {
        use crate::session::Session;
        use crate::ui::devices::DeviceViewState;
        use rlive_core::{ParamId, ParamInfo, TrackId};
        use rlive_engine::create_engine;
        use rlive_plugin_host::scanner::PluginCache;
        use rlive_project::{Device, Project, Track, TrackKind};

        let (_engine, handle) = create_engine(48_000.0, 512);
        let mut p = Project::empty();
        let t = Track::new("T", TrackKind::Midi, [1, 2, 3], p.scenes.len());
        let id: TrackId = t.id;
        p.tracks = vec![t];
        let mut s = Session::new(handle, p, PluginCache::default());
        let chain = ChainRef::Track(id);
        s.add_device(chain, None, Device::builtin("rlive.compressor", "C"));
        let device = s.devices(chain)[0].id;

        let mut queue = ActionQueue::default();
        {
            let dev = &s.devices(chain)[0];
            let rt = &s.runtimes[&device];
            let mut ctx = PanelCtx::new(chain, dev, rt, None, &mut queue, 0.016);
            let threshold = ParamId(0);
            assert_eq!(ctx.value(threshold), Some(-18.0));
            assert!(ctx
                .param(threshold)
                .is_some_and(|p: &ParamInfo| p.name == "Threshold"));
            ctx.set(threshold, -6.0);
            assert_eq!(
                ctx.value(threshold),
                Some(-18.0),
                "not until after the frame"
            );
        }
        queue.apply(&mut s, &mut DeviceViewState::default(), &mut String::new());
        assert_eq!(s.devices(chain)[0].params.get(&0), Some(&-6.0));
    }

    #[test]
    fn an_unknown_panel_reserves_its_placeholder() {
        assert_eq!(size("no_such_panel"), MISSING_SIZE);
    }
}
