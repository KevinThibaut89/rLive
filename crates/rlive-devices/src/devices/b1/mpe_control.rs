//! MPE Control: a passthrough until this device is built.

crate::devices::stub_device!(
    "rlive.mpe_control",
    "MPE Control",
    "MIDI Effects",
    MidiEffect
);
