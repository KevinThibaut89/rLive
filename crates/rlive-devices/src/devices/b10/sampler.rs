//! Sampler. Its implementation is in [`crate::sampler`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, SampleSlot, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.sampler",
    name: "Sampler",
    role: DeviceRole::Instrument,
    category: "Samplers",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::sampler::params(),
        samples: vec![SampleSlot { name: "Sample" }],
        ..Default::default()
    },
    create: |init| {
        Box::new(crate::sampler::Sampler::new(
            init.samples.first().cloned().flatten(),
        ))
    },
};
