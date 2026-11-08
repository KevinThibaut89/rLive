//! Poly Synth, rLive's own device. Its implementation is in [`crate::synth`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.poly",
    name: "Poly Synth",
    role: DeviceRole::Instrument,
    category: "Synths",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::synth::params(),
        ..Default::default()
    },
    create: |_| Box::new(crate::synth::PolySynth::new()),
};
