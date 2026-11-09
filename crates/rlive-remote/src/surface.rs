//! Control surfaces (manual pp. 866-867): up to six, each a script with named input and output
//! ports, whose controls follow the selected device unless locked to one.
//!
//! A script is a [`ControlSurface`]. The simple kind just lists its knobs and leaves the rest to
//! [`ControlSurface::assign`]'s default. A native surface such as Push (A18) claims the controls it
//! wants with [`ControlSurface::claims`], handles them in [`ControlSurface::on_midi`], and draws
//! itself from [`ControlSurface::poll`].

use crate::decode::MidiEvent;
use rlive_core::param::ParamRef;
use rlive_core::DeviceId;
use rlive_project::MidiSource;

/// How many control surfaces can be in use at once.
pub const MAX_SURFACES: usize = 6;

/// A physical control, as a script declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstantControl {
    pub channel: u8,
    pub source: MidiSource,
}

/// One parameter a device offers a surface.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceParam {
    pub target: ParamRef,
    pub name: String,
}

/// The device a surface is controlling.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceDevice {
    pub id: DeviceId,
    pub name: String,
    pub params: Vec<SurfaceParam>,
}

/// A control doing a job for now, until the device changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assignment {
    pub channel: u8,
    pub source: MidiSource,
    pub target: ParamRef,
}

/// A script: what a kind of hardware can do.
pub trait ControlSurface: Send {
    /// The name shown in the chooser.
    fn name(&self) -> &str;

    /// Whether this surface can be locked to a device.
    fn supports_locking(&self) -> bool {
        true
    }

    /// What to send to set the hardware up, if it needs setting up (the Dump button).
    fn preset_dump(&self) -> Option<Vec<Vec<u8>>> {
        None
    }

    /// The knobs that follow the selected device.
    fn instant_controls(&self) -> Vec<InstantControl> {
        Vec::new()
    }

    /// Controls the script handles itself: they reach [`Self::on_midi`] and never a track.
    fn claims(&self) -> Vec<InstantControl> {
        Vec::new()
    }

    /// The controls' current jobs. By default each knob gets the next parameter, one bank of
    /// them at a time.
    fn assign(&self, device: &SurfaceDevice, bank: usize) -> Vec<Assignment> {
        let controls = self.instant_controls();
        let per_bank = controls.len().max(1);
        let first = bank * per_bank;
        controls
            .iter()
            .zip(device.params.iter().skip(first))
            .map(|(c, p)| Assignment {
                channel: c.channel,
                source: c.source,
                target: p.target,
            })
            .collect()
    }

    /// A message from a control the script claimed. `out` sends bytes back to the hardware.
    fn on_midi(&mut self, _event: MidiEvent, _out: &mut dyn FnMut(&[u8])) -> SurfaceAction {
        SurfaceAction::None
    }

    /// A System Exclusive message from the hardware.
    fn on_sysex(&mut self, _bytes: &[u8], _out: &mut dyn FnMut(&[u8])) {}

    /// Called every frame, for a script to update what the hardware shows.
    fn poll(&mut self, _device: Option<&SurfaceDevice>, _out: &mut dyn FnMut(&[u8])) {}
}

/// What a script wants done after a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAction {
    None,
    BankUp,
    BankDown,
}

/// One of the six rows in the settings.
pub struct SurfaceSlot {
    pub surface: Box<dyn ControlSurface>,
    pub input: Option<String>,
    pub output: Option<String>,
    /// The device this surface is locked to, whatever is selected.
    pub locked: Option<DeviceId>,
    /// Which page of a device's parameters the knobs are on.
    pub bank: usize,
}

/// A knob box with no script of its own: eight absolute knobs, CC 21-28 on channel 1, and two
/// buttons, CC 102 and 103, that page through the device's parameters.
#[derive(Debug, Default)]
pub struct GenericKnobs;

/// CC of the first knob on [`GenericKnobs`].
pub const GENERIC_FIRST_KNOB: u8 = 21;
/// CC of the bank down and up buttons on [`GenericKnobs`].
pub const GENERIC_BANK_DOWN: u8 = 102;
pub const GENERIC_BANK_UP: u8 = 103;

impl GenericKnobs {
    const NAME: &'static str = "Generic 8 Knobs";

    fn new_boxed() -> Box<dyn ControlSurface> {
        Box::new(Self)
    }
}

impl ControlSurface for GenericKnobs {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn instant_controls(&self) -> Vec<InstantControl> {
        (0..8)
            .map(|i| InstantControl {
                channel: 0,
                source: MidiSource::Cc {
                    number: GENERIC_FIRST_KNOB + i,
                    encoding: rlive_project::CcEncoding::Absolute,
                },
            })
            .collect()
    }

    fn claims(&self) -> Vec<InstantControl> {
        [GENERIC_BANK_DOWN, GENERIC_BANK_UP]
            .into_iter()
            .map(|number| InstantControl {
                channel: 0,
                source: MidiSource::Cc {
                    number,
                    encoding: rlive_project::CcEncoding::Absolute,
                },
            })
            .collect()
    }

    fn on_midi(&mut self, event: MidiEvent, _out: &mut dyn FnMut(&[u8])) -> SurfaceAction {
        match event {
            MidiEvent::Cc {
                number: GENERIC_BANK_DOWN,
                value,
                ..
            } if value >= 64 => SurfaceAction::BankDown,
            MidiEvent::Cc {
                number: GENERIC_BANK_UP,
                value,
                ..
            } if value >= 64 => SurfaceAction::BankUp,
            _ => SurfaceAction::None,
        }
    }
}

type Script = (&'static str, fn() -> Box<dyn ControlSurface>);

/// The scripts on offer. Native surfaces join this list.
const SCRIPTS: &[Script] = &[(GenericKnobs::NAME, GenericKnobs::new_boxed)];

/// The six slots of control surfaces.
#[derive(Default)]
pub struct SurfaceHost {
    pub slots: [Option<SurfaceSlot>; MAX_SURFACES],
}

impl SurfaceHost {
    /// The names of the scripts that can be chosen.
    pub fn available() -> Vec<&'static str> {
        SCRIPTS.iter().map(|(n, _)| *n).collect()
    }

    /// Puts a script in a row, or clears the row with `None`.
    pub fn set(&mut self, index: usize, script: Option<&str>) {
        let Some(row) = self.slots.get_mut(index) else {
            return;
        };
        *row = script.and_then(|name| {
            let make = SCRIPTS.iter().find(|(n, _)| *n == name)?.1;
            Some(SurfaceSlot {
                surface: make(),
                input: None,
                output: None,
                locked: None,
                bank: 0,
            })
        });
    }

    /// Fills a row with a script made elsewhere (tests, and scripts from a plug-in later on).
    pub fn insert(&mut self, index: usize, surface: Box<dyn ControlSurface>) {
        if let Some(row) = self.slots.get_mut(index) {
            *row = Some(SurfaceSlot {
                surface,
                input: None,
                output: None,
                locked: None,
                bank: 0,
            });
        }
    }

    pub fn set_ports(&mut self, index: usize, input: Option<String>, output: Option<String>) {
        if let Some(Some(row)) = self.slots.get_mut(index) {
            row.input = input;
            row.output = output;
        }
    }

    /// Locks a row to a device, or unlocks it. A surface that can't be locked stays as it is.
    pub fn lock(&mut self, index: usize, device: Option<DeviceId>) {
        if let Some(Some(row)) = self.slots.get_mut(index) {
            if row.surface.supports_locking() {
                row.locked = device;
            }
        }
    }

    /// Whether any surface is locked to this device.
    pub fn is_locked(&self, device: DeviceId) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|s| s.locked == Some(device))
    }

    /// Forget locks to a device that has gone.
    pub fn retain_locks(&mut self, exists: impl Fn(DeviceId) -> bool) {
        for row in self.slots.iter_mut().flatten() {
            if row.locked.is_some_and(|d| !exists(d)) {
                row.locked = None;
            }
        }
    }

    /// The rows that are in use, with their index.
    pub fn in_use(&self) -> impl Iterator<Item = (usize, &SurfaceSlot)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (i, s)))
    }

    /// Runs a script's message handling, and acts on what it asks for.
    pub fn on_midi(&mut self, index: usize, event: MidiEvent, out: &mut dyn FnMut(&[u8])) {
        let Some(Some(row)) = self.slots.get_mut(index) else {
            return;
        };
        match row.surface.on_midi(event, out) {
            SurfaceAction::None => {}
            SurfaceAction::BankUp => row.bank += 1,
            SurfaceAction::BankDown => row.bank = row.bank.saturating_sub(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_core::param::{ParamId, ParamOwner};

    fn device(params: usize) -> SurfaceDevice {
        SurfaceDevice {
            id: DeviceId(7),
            name: "Dev".into(),
            params: (0..params)
                .map(|i| SurfaceParam {
                    target: ParamRef {
                        owner: ParamOwner::Device(DeviceId(7)),
                        id: ParamId(i as u32),
                    },
                    name: format!("P{i}"),
                })
                .collect(),
        }
    }

    #[test]
    fn six_named_rows_at_most() {
        let mut host = SurfaceHost::default();
        for i in 0..MAX_SURFACES {
            host.set(i, Some(GenericKnobs::NAME));
        }
        host.set(MAX_SURFACES, Some(GenericKnobs::NAME));
        assert_eq!(host.in_use().count(), MAX_SURFACES);
        assert_eq!(
            host.slots[0].as_ref().unwrap().surface.name(),
            "Generic 8 Knobs"
        );
        host.set(0, None);
        assert_eq!(host.in_use().count(), MAX_SURFACES - 1);
    }

    #[test]
    fn knobs_take_the_next_bank_of_parameters() {
        let s = GenericKnobs;
        let dev = device(20);
        let bank0 = s.assign(&dev, 0);
        assert_eq!(bank0.len(), 8);
        assert_eq!(bank0[0].target, dev.params[0].target);
        let bank2 = s.assign(&dev, 2);
        assert_eq!(bank2.len(), 4, "only 20 parameters");
        assert_eq!(bank2[0].target, dev.params[16].target);
    }

    #[test]
    fn locks_follow_the_device_and_forget_it_when_it_goes() {
        let mut host = SurfaceHost::default();
        host.set(0, Some(GenericKnobs::NAME));
        host.lock(0, Some(DeviceId(7)));
        assert!(host.is_locked(DeviceId(7)));
        host.retain_locks(|_| false);
        assert!(!host.is_locked(DeviceId(7)));
    }

    struct Fixed;
    impl ControlSurface for Fixed {
        fn name(&self) -> &str {
            "Fixed"
        }
        fn supports_locking(&self) -> bool {
            false
        }
        fn preset_dump(&self) -> Option<Vec<Vec<u8>>> {
            Some(vec![vec![0xf0, 1, 0xf7]])
        }
    }

    #[test]
    fn surfaces_without_locking_ignore_it_and_scripts_can_dump() {
        let mut host = SurfaceHost::default();
        host.insert(1, Box::new(Fixed));
        host.lock(1, Some(DeviceId(7)));
        assert!(!host.is_locked(DeviceId(7)));
        assert_eq!(
            host.slots[1].as_ref().unwrap().surface.preset_dump(),
            Some(vec![vec![0xf0, 1, 0xf7]])
        );
    }

    #[test]
    fn bank_buttons_page_the_knobs() {
        let mut host = SurfaceHost::default();
        host.set(0, Some(GenericKnobs::NAME));
        let cc = |number, value| MidiEvent::Cc {
            channel: 0,
            number,
            value,
        };
        host.on_midi(0, cc(GENERIC_BANK_UP, 127), &mut |_| {});
        assert_eq!(host.slots[0].as_ref().unwrap().bank, 1);
        host.on_midi(0, cc(GENERIC_BANK_DOWN, 127), &mut |_| {});
        host.on_midi(0, cc(GENERIC_BANK_DOWN, 127), &mut |_| {});
        assert_eq!(
            host.slots[0].as_ref().unwrap().bank,
            0,
            "stays at the start"
        );
    }
}
