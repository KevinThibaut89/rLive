//! Delay. Its implementation is in [`crate::effects`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.delay",
    name: "Delay",
    role: DeviceRole::AudioEffect,
    category: "Delay & Modulation",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::delay_params(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Delay>::default(),
};
