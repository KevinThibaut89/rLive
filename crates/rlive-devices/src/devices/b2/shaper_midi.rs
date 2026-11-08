//! Shaper MIDI: a passthrough until this device is built.

crate::devices::stub_device!("rlive.shaper_midi", "Shaper MIDI", "Modulators", MidiEffect, mod_out: ["Output"]);
