//! How a device declares itself: what it controls, how it connects and how it is laid out.
//!
//! The UI and the engine read a [`DeviceDesc`]; a device supplies one and never sees either.

use crate::param::{ParamId, ParamInfo};
use crate::view::ViewId;

/// How far along a built-in device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// A passthrough placeholder, hidden from the browser.
    Stub,
    /// Works, but has not been audited against the manual.
    Beta,
    /// Audited against the manual.
    Done,
}

/// The full declaration of a device.
#[derive(Debug, Clone, Default)]
pub struct DeviceDesc {
    pub params: Vec<ParamInfo>,
    pub ports: Ports,
    /// Sections of the device's panel. Empty gets one untitled section of knobs, one for each
    /// parameter, like a plugin's.
    pub layout: Vec<Section>,
    /// What can be loaded into the device. An entry's position is its resource slot.
    pub samples: Vec<SampleSlot>,
}

/// What a device can connect to, beyond its main stereo pair and MIDI input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ports {
    pub sidechain: bool,
    /// Writes MIDI to `events_out` for the next device.
    pub midi_out: bool,
    /// Extra stereo output buses, by name.
    pub aux_out: Vec<&'static str>,
    /// Modulation sources, by name.
    pub mod_out: Vec<&'static str>,
}

/// A titled group of controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub items: Vec<Item>,
}

/// One element of a device's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Knob(ParamId),
    Slider(ParamId),
    Toggle(ParamId),
    Menu(ParamId),
    /// A stereo level meter, reading [`METER_VALUES`](crate::METER_VALUES) values at its view.
    Meter(ViewId),
    /// A scope, reading [`SCOPE_VALUES`](crate::SCOPE_VALUES) values at its view.
    Scope(ViewId, ScopeKind),
    Row(Vec<Item>),
    /// A custom panel, found by name in the table in `rlive-app`'s `ui/panels`.
    Panel(&'static str),
}

/// How a scope draws its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Waveform,
    Spectrum,
    Vectorscope,
}

/// A place a device can load a sample into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleSlot {
    pub name: &'static str,
}
