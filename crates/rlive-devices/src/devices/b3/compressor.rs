//! Compressor. Its implementation is in [`crate::effects`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.compressor",
    name: "Compressor",
    role: DeviceRole::AudioEffect,
    category: "Dynamics",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::compressor_params(),
        layout: crate::effects::compressor::compressor_layout(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Compressor>::default(),
};
