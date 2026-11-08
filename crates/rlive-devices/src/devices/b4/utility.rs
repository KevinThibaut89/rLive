//! Utility. Its implementation is in [`crate::effects`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.utility",
    name: "Utility",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::utility_params(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Utility>::default(),
};
