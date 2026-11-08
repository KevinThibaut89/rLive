//! EQ Three. Its implementation is in [`crate::effects`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.eq3",
    name: "EQ Three",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::eq_params(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Eq3>::default(),
};
