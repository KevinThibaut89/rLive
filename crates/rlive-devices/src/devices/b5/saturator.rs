//! Saturator. Its implementation is in [`crate::effects`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.saturator",
    name: "Saturator",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::saturator_params(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Saturator>::default(),
};
