//! Remote control: reading what a controller sends and acting on it, and what a control surface is.
//!
//! This crate is pure logic, with no MIDI backend, UI or engine in it, so all of it can be tested
//! by feeding it bytes. It has three layers:
//!
//! * [`decode`] and [`takeover`]: what one message means for one kind of control.
//! * [`table`]: the compiled mappings the MIDI thread looks messages up in. A lookup never takes
//!   a lock or allocates, and tells a [`table::RemoteSink`] what happened.
//! * [`surface`]: up to six named control surfaces, and instant mappings following a device.
//!
//! [`learn`] works out which message a control should be mapped to, and [`feedback`] what to send
//! back to LEDs and motor faders.

pub mod decode;
pub mod feedback;
pub mod learn;
pub mod surface;
pub mod table;
pub mod takeover;

pub use decode::{decode_relative, MidiEvent, Reading};
pub use feedback::{FeedbackSink, FeedbackState, FeedbackValue};
pub use learn::{Learned, Learner};
pub use surface::{
    Assignment, ControlSurface, GenericKnobs, InstantControl, SurfaceDevice, SurfaceHost,
    SurfaceParam, SurfaceSlot, MAX_SURFACES,
};
pub use table::{
    CellAddr, Compiled, Exec, Gesture, ParamShape, RemoteSink, RemoteTable, ShapeKind, UiKind,
    ALL_PORTS, RELATIVE_STEP,
};
pub use takeover::{Takeover, TakeoverMode};
