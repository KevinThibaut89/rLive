//! Runtime parameter state: one flat bus of cells per track, resolved every control step.
//!
//! Every mixer control and device parameter of a track owns a [`ParamCell`] in the track's
//! [`ParamBus`]. Each control step ([`CONTROL_STEP`] frames) resolves the cells whose value may
//! have changed (manual p. 537: automation is absolute, modulation only influences that value):
//!
//! 1. A Macro writes `auto` and owns the parameter ([`ControlHooks::macros`]).
//! 2. Automation writes `auto` unless overridden ([`ControlHooks::automation`]).
//! 3. Modulators and clip envelopes set `add` and `scale` ([`ControlHooks::modulation`]).
//! 4. `effective = clamp(norm(auto or base) * scale + add)`, converted to plain units and
//!    delivered if changed.
//!
//! Working in normalized space keeps VST3's normalized values and CLAP's plain values uniform.
//! A cell's identity is its index: cells never move for the life of a track, so a [`CellIx`]
//! stays valid across device insert, remove and move, and across bus swaps.
//!
//! [`BusLayout`] is the UI-side plan of who owns which cell; the engine only sees the finished
//! [`ParamBus`].

use crate::shared::AtomicF32;
use crate::track::MixSettings;
use rlive_core::param::{mixer, ModMode, ParamId, ParamRange, ACTIVATOR};
use rlive_core::{DeviceId, ParamInfo};
use std::collections::HashMap;
use std::sync::Arc;

/// Frames between parameter resolves.
pub const CONTROL_STEP: usize = 32;

/// A cell's position in its track's bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellIx(pub u32);

/// Cells of the mixer controls. Every track's bus starts with these; the Master has only volume.
pub mod mix_cell {
    use super::CellIx;

    pub const VOLUME: CellIx = CellIx(0);
    pub const PAN: CellIx = CellIx(1);
    pub const MUTE: CellIx = CellIx(2);
    pub const SOLO: CellIx = CellIx(3);
}

const DIRTY: u8 = 1;
/// A Macro owns the parameter: `auto` is the source, whatever else is going on.
const MACRO: u8 = 1 << 1;
/// Automation is driving the parameter.
const AUTOMATED: u8 = 1 << 2;
/// The user took over from automation; `base` is the source until re-enabled.
const OVERRIDDEN: u8 = 1 << 3;

/// One parameter's live state. `base` and `auto` are plain units; everything derived from them
/// goes through normalized space.
#[derive(Debug, Clone, Copy)]
struct ParamCell {
    /// The knob, as set by the user or the plugin.
    base: f64,
    /// The value written by a Macro or automation.
    auto: f64,
    add: f32,
    scale: f32,
    flags: u8,
    /// The last value handed to delivery, in plain units.
    sent: f64,
}

/// Which object a resolved value goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellTarget {
    Free,
    Mixer(ParamId),
    Device {
        device: DeviceId,
        param: ParamId,
    },
    /// The device's on/off switch ([`ACTIVATOR`](rlive_core::ACTIVATOR)).
    Activator {
        device: DeviceId,
    },
}

/// The part of a cell the audio thread needs to convert its value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSpec {
    pub range: ParamRange,
    pub mod_mode: ModMode,
    pub target: CellTarget,
}

impl CellSpec {
    const FREE: CellSpec = CellSpec {
        range: ParamRange {
            min: 0.0,
            max: 1.0,
            skew: 1.0,
            quantized: false,
        },
        mod_mode: ModMode::Additive,
        target: CellTarget::Free,
    };

    fn of(info: &ParamInfo, target: CellTarget) -> Self {
        Self {
            range: info.range(),
            mod_mode: info.mod_mode,
            target,
        }
    }
}

/// What resolving a bus found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// A device must hear a change, so its chain has to stop at this step.
    pub devices: bool,
    /// A mixer value changed; only the gain stage cares.
    pub mixer: bool,
}

/// The flat parameter bus of a track or the Master.
pub struct ParamBus {
    cells: Box<[ParamCell]>,
    specs: Box<[CellSpec]>,
    /// Normalized effective values, for the UI.
    effective: Arc<[AtomicF32]>,
    /// Cells to resolve, then cells to deliver. Never outgrows `cells`: a cell queues once.
    queue: Box<[u32]>,
    queued: usize,
    /// Sorted index for parameter changes that come from the device itself.
    by_param: Box<[(DeviceId, u32, CellIx)]>,
}

impl ParamBus {
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// How a cell converts its value, and how modulation applies to it.
    pub fn spec(&self, cell: CellIx) -> Option<CellSpec> {
        self.specs.get(cell.0 as usize).copied()
    }

    /// The user-facing value of a cell, in plain units.
    pub fn base(&self, cell: CellIx) -> Option<f64> {
        self.cells.get(cell.0 as usize).map(|c| c.base)
    }

    /// The normalized value last resolved for a cell.
    pub fn effective(&self, cell: CellIx) -> Option<f32> {
        self.effective.get(cell.0 as usize).map(|v| v.load())
    }

    #[inline]
    fn touch(&mut self, ix: usize) {
        let flags = &mut self.cells[ix].flags;
        if *flags & DIRTY == 0 {
            *flags |= DIRTY;
            self.queue[self.queued] = ix as u32;
            self.queued += 1;
        }
    }

    /// Set the knob of a cell. A `gesture` means the user moved it, which overrides automation.
    pub fn set_base(&mut self, cell: CellIx, value: f64, gesture: bool) {
        let ix = cell.0 as usize;
        if ix >= self.cells.len() || !value.is_finite() {
            return;
        }
        let c = &mut self.cells[ix];
        c.base = value;
        if gesture && c.flags & AUTOMATED != 0 {
            c.flags |= OVERRIDDEN;
        }
        self.touch(ix);
    }

    /// Step 1: a Macro drives this cell and owns it until [`release_macro`](Self::release_macro).
    pub fn write_macro(&mut self, cell: CellIx, value: f64) {
        let ix = cell.0 as usize;
        if ix >= self.cells.len() || !value.is_finite() {
            return;
        }
        let c = &mut self.cells[ix];
        c.auto = value;
        c.flags |= MACRO;
        self.touch(ix);
    }

    pub fn release_macro(&mut self, cell: CellIx) {
        let ix = cell.0 as usize;
        if ix < self.cells.len() && self.cells[ix].flags & MACRO != 0 {
            self.cells[ix].flags &= !MACRO;
            self.touch(ix);
        }
    }

    /// Step 2: automation's value for this step. Ignored while a Macro owns the cell or the
    /// user has overridden it, so it holds no opinion until [`re_enable`](Self::re_enable).
    pub fn write_automation(&mut self, cell: CellIx, value: f64) {
        let ix = cell.0 as usize;
        if ix >= self.cells.len() || !value.is_finite() {
            return;
        }
        let c = &mut self.cells[ix];
        c.flags |= AUTOMATED;
        if c.flags & (MACRO | OVERRIDDEN) == 0 {
            c.auto = value;
            self.touch(ix);
        }
    }

    /// Stop automating a cell, returning it to its knob.
    pub fn clear_automation(&mut self, cell: CellIx) {
        let ix = cell.0 as usize;
        if ix < self.cells.len() && self.cells[ix].flags & (AUTOMATED | OVERRIDDEN) != 0 {
            self.cells[ix].flags &= !(AUTOMATED | OVERRIDDEN);
            self.touch(ix);
        }
    }

    /// Step 3: set a cell's modulation. `add` is in normalized units; `scale` multiplies.
    pub fn modulate(&mut self, cell: CellIx, add: f32, scale: f32) {
        let ix = cell.0 as usize;
        if ix >= self.cells.len() || !add.is_finite() || !scale.is_finite() {
            return;
        }
        let c = &mut self.cells[ix];
        if c.add != add || c.scale != scale {
            c.add = add;
            c.scale = scale;
            self.touch(ix);
        }
    }

    /// Hand one cell, or all of them, back to automation.
    pub fn re_enable(&mut self, cell: Option<CellIx>) {
        let (start, end) = match cell {
            Some(c) => (c.0 as usize, (c.0 as usize + 1).min(self.cells.len())),
            None => (0, self.cells.len()),
        };
        for ix in start..end {
            if self.cells[ix].flags & OVERRIDDEN != 0 {
                self.cells[ix].flags &= !OVERRIDDEN;
                self.touch(ix);
            }
        }
    }

    /// A device changed one of its own parameters. It already has the value, so it is
    /// recorded as delivered and not sent back.
    pub fn report_from_device(&mut self, device: DeviceId, param: u32, value: f64) {
        if !value.is_finite() {
            return;
        }
        let Ok(found) = self
            .by_param
            .binary_search_by_key(&(device, param), |&(d, p, _)| (d, p))
        else {
            return;
        };
        let ix = self.by_param[found].2 .0 as usize;
        let spec = self.specs[ix];
        let c = &mut self.cells[ix];
        c.base = value;
        c.sent = spec.range.clamp(value);
        if c.flags & (MACRO | AUTOMATED) == 0 {
            let norm = spec.range.to_normalized(c.sent);
            self.effective[ix].store(norm as f32);
        }
    }

    /// Step 4 for every dirty cell. Leaves the cells that changed queued for delivery.
    pub(crate) fn resolve(&mut self) -> Resolved {
        let mut out = Resolved::default();
        let mut kept = 0;
        for q in 0..self.queued {
            let ix = self.queue[q] as usize;
            let spec = self.specs[ix];
            let c = &mut self.cells[ix];
            c.flags &= !DIRTY;
            if spec.target == CellTarget::Free {
                continue;
            }
            let automated =
                c.flags & MACRO != 0 || (c.flags & AUTOMATED != 0 && c.flags & OVERRIDDEN == 0);
            let source = if automated { c.auto } else { c.base };
            let (norm, plain) = if c.add == 0.0 && c.scale == 1.0 {
                // Without modulation, skip the trip through normalized space: delivered values
                // stay exactly what was set.
                let plain = spec.range.clamp(source);
                (spec.range.to_normalized(plain), plain)
            } else {
                let norm = (spec.range.to_normalized(source) * c.scale as f64 + c.add as f64)
                    .clamp(0.0, 1.0);
                (norm, spec.range.from_normalized(norm))
            };
            self.effective[ix].store(norm as f32);
            if plain != c.sent {
                c.sent = plain;
                self.queue[kept] = ix as u32;
                kept += 1;
                match spec.target {
                    CellTarget::Mixer(_) => out.mixer = true,
                    _ => out.devices = true,
                }
            }
        }
        self.queued = kept;
        out
    }

    /// Hand every resolved change to `deliver`, after [`resolve`](Self::resolve) queued them.
    pub(crate) fn drain_resolved(&mut self, mut deliver: impl FnMut(CellTarget, f64)) {
        for q in 0..std::mem::take(&mut self.queued) {
            let ix = self.queue[q] as usize;
            deliver(self.specs[ix].target, self.cells[ix].sent);
        }
    }

    /// Take over the live state of a previous bus for cells that kept their owner.
    pub(crate) fn adopt(&mut self, old: &ParamBus) {
        for (ix, spec) in self.specs.iter().enumerate() {
            if spec.target == CellTarget::Free {
                continue;
            }
            let Some(prev) = old.specs.get(ix) else {
                continue;
            };
            if prev.target != spec.target {
                continue;
            }
            self.cells[ix] = old.cells[ix];
            self.effective[ix].store(old.effective[ix].load());
            if self.cells[ix].flags & DIRTY != 0 {
                self.queue[self.queued] = ix as u32;
                self.queued += 1;
            }
        }
    }
}

/// Where a control step falls in the block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlStep {
    /// Steps since the start of the block.
    pub index: u32,
    /// Frames from the start of the block.
    pub offset: u32,
    /// Transport position at this step, in ticks.
    pub tick: f64,
}

/// What writes a bus during its control step, in resolve order. Macros, automation, modulators
/// and clip envelopes implement these later; nothing does yet. Called on the audio thread,
/// so no implementation may allocate, lock or free.
pub trait ControlHooks: Send {
    /// Step 1 of the resolve order.
    fn macros(&mut self, _step: ControlStep, _bus: &mut ParamBus) {}
    /// Step 2 of the resolve order.
    fn automation(&mut self, _step: ControlStep, _bus: &mut ParamBus) {}
    /// Step 3 of the resolve order.
    fn modulation(&mut self, _step: ControlStep, _bus: &mut ParamBus) {}
}

/// Which modulation and Macro sources feed a track's cells. Later milestones fill this in.
#[derive(Debug, Default)]
pub struct TrackRoutes {}

struct DeviceCells {
    activator: CellIx,
    /// Activator included.
    len: u32,
    params: HashMap<u32, CellIx>,
}

/// UI-side plan of one track's bus: who owns which cell, found in O(1).
pub struct BusLayout {
    specs: Vec<CellSpec>,
    /// Latest values the UI knows of, seeding the cells of a new bus.
    base: Vec<f64>,
    free: Vec<(u32, u32)>,
    devices: HashMap<DeviceId, DeviceCells>,
    effective: Arc<[AtomicF32]>,
}

impl BusLayout {
    fn with_mixer(controls: &[(ParamInfo, f64)]) -> Self {
        let specs: Vec<CellSpec> = controls
            .iter()
            .map(|(info, _)| CellSpec::of(info, CellTarget::Mixer(info.param_id())))
            .collect();
        let base = controls.iter().map(|(_, value)| *value).collect();
        Self {
            specs,
            base,
            free: Vec::new(),
            devices: HashMap::new(),
            effective: Arc::from([]),
        }
    }

    /// A track's layout: the mixer controls and no devices yet.
    pub fn for_track(mix: &MixSettings) -> Self {
        let [volume, pan, mute, solo] = mixer::infos();
        Self::with_mixer(&[
            (volume, mix.volume_db as f64),
            (pan, mix.pan as f64),
            (mute, mix.mute as u8 as f64),
            (solo, mix.solo as u8 as f64),
        ])
    }

    /// The Master's layout: its volume and no devices yet.
    pub fn for_master(volume_db: f32) -> Self {
        let [volume, ..] = mixer::infos();
        Self::with_mixer(&[(volume, volume_db as f64)])
    }

    fn take_range(&mut self, len: u32) -> u32 {
        if let Some(i) = self.free.iter().position(|&(_, l)| l >= len) {
            let (start, l) = self.free[i];
            if l == len {
                self.free.remove(i);
            } else {
                self.free[i] = (start + len, l - len);
            }
            return start;
        }
        let start = self.specs.len() as u32;
        self.specs
            .resize(self.specs.len() + len as usize, CellSpec::FREE);
        self.base.resize(self.specs.len(), 0.0);
        start
    }

    fn give_back(&mut self, start: u32, len: u32) {
        let range = start as usize..(start + len) as usize;
        self.specs[range.clone()].fill(CellSpec::FREE);
        self.base[range].fill(0.0);
        let at = self.free.partition_point(|&(s, _)| s < start);
        self.free.insert(at, (start, len));
        // Join neighbours so ranges don't fragment over a long session.
        if at + 1 < self.free.len() && start + len == self.free[at + 1].0 {
            self.free[at].1 += self.free[at + 1].1;
            self.free.remove(at + 1);
        }
        if at > 0 && self.free[at - 1].0 + self.free[at - 1].1 == start {
            self.free[at - 1].1 += self.free[at].1;
            self.free.remove(at);
        }
    }

    /// Give a device its cells, or refit the ones it has. Values come from `value`. Returns
    /// whether the engine needs a new bus built from this layout.
    pub fn place_device(
        &mut self,
        device: DeviceId,
        on: bool,
        params: &[ParamInfo],
        value: impl Fn(&ParamInfo) -> f64,
    ) -> bool {
        if let Some(d) = self.devices.get(&device) {
            let unchanged = d.len as usize == params.len() + 1
                && params.iter().enumerate().all(|(i, p)| {
                    let ix = (d.activator.0 + 1 + i as u32) as usize;
                    d.params.get(&p.id) == Some(&CellIx(ix as u32))
                        && self.specs[ix].range == p.range()
                });
            if unchanged {
                return false;
            }
            self.remove_device(device);
        }
        let len = params.len() as u32 + 1;
        let start = self.take_range(len);
        let activator = ParamInfo::toggle(ACTIVATOR.0, "Device On", true);
        self.specs[start as usize] = CellSpec::of(&activator, CellTarget::Activator { device });
        self.base[start as usize] = on as u8 as f64;
        let mut by_id = HashMap::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let ix = start as usize + 1 + i;
            let target = CellTarget::Device {
                device,
                param: p.param_id(),
            };
            self.specs[ix] = CellSpec::of(p, target);
            self.base[ix] = value(p);
            by_id.insert(p.id, CellIx(ix as u32));
        }
        self.devices.insert(
            device,
            DeviceCells {
                activator: CellIx(start),
                len,
                params: by_id,
            },
        );
        true
    }

    /// Release a device's cells. The bus keeps them idle until a later bus reuses them.
    pub fn remove_device(&mut self, device: DeviceId) {
        if let Some(d) = self.devices.remove(&device) {
            self.give_back(d.activator.0, d.len);
        }
    }

    /// The cell of a device parameter.
    pub fn device_cell(&self, device: DeviceId, param: u32) -> Option<CellIx> {
        self.devices.get(&device)?.params.get(&param).copied()
    }

    /// The cell of a device's on/off switch.
    pub fn activator_cell(&self, device: DeviceId) -> Option<CellIx> {
        self.devices.get(&device).map(|d| d.activator)
    }

    /// The latest value the UI knows of for a cell.
    pub fn base(&self, cell: CellIx) -> Option<f64> {
        self.base.get(cell.0 as usize).copied()
    }

    /// Record a value the UI set or a device reported.
    pub fn note_value(&mut self, cell: CellIx, value: f64) {
        if let Some(v) = self.base.get_mut(cell.0 as usize) {
            *v = value;
        }
    }

    /// The normalized effective value of a cell, as last resolved.
    pub fn effective(&self, cell: CellIx) -> Option<f32> {
        self.effective.get(cell.0 as usize).map(|v| v.load())
    }

    /// Build a bus of this layout, seeded with the current values. The engine keeps the live
    /// state of cells it already had. Call on the UI thread.
    pub fn build(&mut self) -> Box<ParamBus> {
        let effective: Arc<[AtomicF32]> = self
            .specs
            .iter()
            .zip(&self.base)
            .map(|(spec, value)| AtomicF32::new(spec.range.to_normalized(*value) as f32))
            .collect();
        self.effective = effective.clone();
        let cells: Box<[ParamCell]> = self
            .specs
            .iter()
            .zip(&self.base)
            .map(|(spec, &value)| ParamCell {
                base: value,
                auto: value,
                add: 0.0,
                scale: 1.0,
                flags: 0,
                sent: spec.range.clamp(value),
            })
            .collect();
        let mut by_param: Vec<(DeviceId, u32, CellIx)> = self
            .specs
            .iter()
            .enumerate()
            .filter_map(|(ix, spec)| match spec.target {
                CellTarget::Device { device, param } => Some((device, param.0, CellIx(ix as u32))),
                _ => None,
            })
            .collect();
        by_param.sort_unstable();
        Box::new(ParamBus {
            queue: vec![0; cells.len()].into_boxed_slice(),
            queued: 0,
            specs: self.specs.clone().into_boxed_slice(),
            by_param: by_param.into_boxed_slice(),
            effective,
            cells,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (BusLayout, DeviceId, CellIx) {
        let mut l = BusLayout::for_track(&MixSettings::default());
        let dev = DeviceId::new();
        let params = [ParamInfo::continuous(0, "Mix", 0.0, 10.0, 5.0, "")];
        l.place_device(dev, true, &params, |p| p.default);
        let cell = l.device_cell(dev, 0).unwrap();
        (l, dev, cell)
    }

    fn resolve(bus: &mut ParamBus) -> Vec<(CellTarget, f64)> {
        bus.resolve();
        let mut out = Vec::new();
        bus.drain_resolved(|t, v| out.push((t, v)));
        out
    }

    #[test]
    fn resolve_uses_the_knob_when_nothing_else_applies() {
        let (mut l, dev, cell) = layout();
        let mut bus = l.build();
        bus.set_base(cell, 2.5, true);
        let delivered = resolve(&mut bus);
        let target = CellTarget::Device {
            device: dev,
            param: ParamId(0),
        };
        assert_eq!(delivered, [(target, 2.5)]);
        assert!((bus.effective(cell).unwrap() - 0.25).abs() < 1e-6);
        assert!(resolve(&mut bus).is_empty(), "no change, nothing delivered");
    }

    #[test]
    fn resolving_an_unchanged_value_delivers_nothing() {
        let (mut l, _, cell) = layout();
        let mut bus = l.build();
        // The device already has its default; this must not be sent again.
        bus.set_base(cell, 5.0, true);
        assert!(resolve(&mut bus).is_empty());
    }

    #[test]
    fn automation_overrides_the_knob_until_a_gesture() {
        let (mut l, _, cell) = layout();
        let mut bus = l.build();
        bus.write_automation(cell, 8.0);
        assert_eq!(resolve(&mut bus)[0].1, 8.0);

        // A programmatic set changes the knob but automation stays in charge.
        bus.set_base(cell, 3.0, false);
        assert!(resolve(&mut bus).is_empty());

        // The user grabs the knob: override.
        bus.set_base(cell, 4.0, true);
        assert_eq!(resolve(&mut bus)[0].1, 4.0);
        bus.write_automation(cell, 9.0);
        assert!(resolve(&mut bus).is_empty(), "overridden");

        // Re-enable hands it back; automation resumes at its next write.
        bus.re_enable(Some(cell));
        bus.write_automation(cell, 9.0);
        assert_eq!(resolve(&mut bus)[0].1, 9.0);
    }

    #[test]
    fn macros_outrank_automation() {
        let (mut l, _, cell) = layout();
        let mut bus = l.build();
        bus.write_macro(cell, 7.0);
        bus.write_automation(cell, 2.0);
        assert_eq!(resolve(&mut bus)[0].1, 7.0);
        bus.release_macro(cell);
        bus.write_automation(cell, 2.0);
        assert_eq!(resolve(&mut bus)[0].1, 2.0);
    }

    #[test]
    fn scale_and_add_apply_in_normalized_space_and_clamp() {
        let mut l = BusLayout::for_track(&MixSettings::default());
        let dev = DeviceId::new();
        // A skewed range: normalized and plain values differ.
        let params = [ParamInfo::skewed(0, "F", 1.0, 101.0, 26.0, "", 2.0)];
        l.place_device(dev, true, &params, |p| p.default);
        let cell = l.device_cell(dev, 0).unwrap();
        let mut bus = l.build();
        let norm = params[0].to_normalized(26.0);

        bus.modulate(cell, 0.0, 0.5);
        assert!(!resolve(&mut bus).is_empty());
        assert!((bus.effective(cell).unwrap() as f64 - norm * 0.5).abs() < 1e-6);

        bus.modulate(cell, 0.1, 1.0);
        let plain = resolve(&mut bus)[0].1;
        let expect = params[0].from_normalized(norm + 0.1);
        assert!((plain - expect).abs() < 1e-3, "{plain} vs {expect}");

        // Both directions clamp to the range.
        bus.modulate(cell, 10.0, 1.0);
        assert_eq!(resolve(&mut bus)[0].1, 101.0);
        assert_eq!(bus.effective(cell), Some(1.0));
        bus.modulate(cell, 0.0, -1.0);
        assert_eq!(resolve(&mut bus)[0].1, 1.0);
        assert_eq!(bus.effective(cell), Some(0.0));

        // Modulation only influences automation's value (manual p. 537).
        bus.modulate(cell, 0.0, 0.5);
        bus.write_automation(cell, 101.0);
        resolve(&mut bus);
        assert_eq!(bus.effective(cell), Some(0.5));
    }

    #[test]
    fn stepped_params_stay_quantized_under_modulation() {
        let mut l = BusLayout::for_track(&MixSettings::default());
        let dev = DeviceId::new();
        let params = [ParamInfo::stepped(0, "Wave", &["A", "B", "C", "D"], 0)];
        l.place_device(dev, true, &params, |p| p.default);
        let cell = l.device_cell(dev, 0).unwrap();
        let mut bus = l.build();
        bus.modulate(cell, 0.45, 1.0);
        assert_eq!(resolve(&mut bus)[0].1, 1.0);
    }

    #[test]
    fn a_plugin_changing_its_own_param_is_not_echoed_back() {
        let (mut l, dev, cell) = layout();
        let mut bus = l.build();
        bus.report_from_device(dev, 0, 9.0);
        assert_eq!(bus.base(cell), Some(9.0));
        assert_eq!(bus.effective(cell), Some(0.9));
        assert!(resolve(&mut bus).is_empty());
        // Setting it to where the device already is sends nothing either.
        bus.set_base(cell, 9.0, true);
        assert!(resolve(&mut bus).is_empty());
    }

    #[test]
    fn adopt_keeps_state_of_cells_that_kept_their_owner() {
        let (mut l, dev, cell) = layout();
        let mut old = l.build();
        old.set_base(cell, 1.0, false);
        old.resolve();
        old.drain_resolved(|_, _| {});

        let extra = DeviceId::new();
        l.place_device(extra, true, &[], |p| p.default);
        let mut new = l.build();
        new.adopt(&old);
        assert_eq!(new.base(cell), Some(1.0));
        assert_eq!(new.effective(cell), Some(0.1));
        assert_eq!(new.len(), old.len() + 1);
        assert_eq!(l.device_cell(dev, 0), Some(cell));
    }

    #[test]
    fn cells_of_a_reused_range_start_fresh() {
        let (mut l, a, cell) = layout();
        let mut old = l.build();
        old.set_base(cell, 1.0, false);
        l.remove_device(a);
        assert_eq!(l.device_cell(a, 0), None);
        let b = DeviceId::new();
        let params = [ParamInfo::continuous(0, "Mix", 0.0, 10.0, 7.0, "")];
        l.place_device(b, true, &params, |p| p.default);
        assert_eq!(l.device_cell(b, 0), Some(cell), "the freed range is reused");
        let mut new = l.build();
        new.adopt(&old);
        assert_eq!(new.base(cell), Some(7.0));
    }

    #[test]
    fn free_ranges_join_and_get_reused() {
        let mut l = BusLayout::for_track(&MixSettings::default());
        let (a, b, c) = (DeviceId::new(), DeviceId::new(), DeviceId::new());
        for d in [a, b, c] {
            l.place_device(d, true, &[], |p| p.default);
        }
        let first = l.activator_cell(a).unwrap();
        l.remove_device(a);
        l.remove_device(b);
        let params = [ParamInfo::toggle(0, "T", false)];
        let d = DeviceId::new();
        l.place_device(d, true, &params, |p| p.default);
        assert_eq!(l.activator_cell(d), Some(first));
    }

    #[test]
    fn replacing_a_device_with_the_same_params_keeps_its_cells() {
        let (mut l, dev, cell) = layout();
        let params = [ParamInfo::continuous(0, "Mix", 0.0, 10.0, 5.0, "")];
        assert!(!l.place_device(dev, true, &params, |p| p.default));
        assert_eq!(l.device_cell(dev, 0), Some(cell));
    }

    #[test]
    fn out_of_range_cells_are_ignored() {
        let (mut l, _, _) = layout();
        let mut bus = l.build();
        let beyond = CellIx(bus.len() as u32);
        bus.set_base(beyond, 1.0, true);
        bus.write_automation(beyond, 1.0);
        bus.modulate(beyond, 0.5, 0.5);
        bus.re_enable(Some(beyond));
        assert!(resolve(&mut bus).is_empty());
    }
}
