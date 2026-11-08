//! Reverb. Its implementation is in [`crate::effects`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.reverb",
    name: "Reverb",
    role: DeviceRole::AudioEffect,
    category: "Reverb & Resonance",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::reverb_params(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Reverb>::default(),
};
