//! Juno-106, a circuit-level emulation outside Live's device set. See [`crate::juno106`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Status};

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.juno106",
    name: "Juno-106",
    role: DeviceRole::Instrument,
    category: "Synths",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::juno106::params(),
        ..Default::default()
    },
    create: |_| Box::new(crate::juno106::Juno106::new()),
};
