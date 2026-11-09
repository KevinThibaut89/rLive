//! External Audio Effect and External Instrument: hardware (or another track's plug-in) as part
//! of a device chain.
//!
//! The devices do little themselves. The engine plays what they put on their one extra output
//! out of the hardware (`aux_out[0]`), and feeds their sidechain from the inputs chosen; see
//! `RenderPlan::{hardware_sends, hardware_returns}`. Where they connect is project state, not a
//! parameter.

pub mod common;
pub mod external_audio_effect;
pub mod external_instrument;
