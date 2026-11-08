//! Envelope MIDI: a passthrough until this device is built.

crate::devices::stub_device!("rlive.envelope_midi", "Envelope MIDI", "Modulators", MidiEffect, mod_out: ["Output"]);
