//! The Drum Synths: one-shot percussion voices.
//!
//! The manual describes each instrument's controls and what it is made of (pp. 831-838), but
//! gives no ranges, envelopes or tunings. Those are ours, chosen for sounds that are recognisably
//! what each instrument is named after. Velocity sets the level of a hit; which key fired it
//! doesn't change the sound.

pub mod common;
pub mod ds_clang;
pub mod ds_clap;
pub mod ds_cymbal;
pub mod ds_fm;
pub mod ds_hh;
pub mod ds_kick;
pub mod ds_snare;
pub mod ds_tom;
