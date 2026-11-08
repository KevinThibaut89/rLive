//! Sampler. Its implementation is in [`crate::sampler`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, ParamId, SampleSlot, Section, Status};

fn slots() -> Vec<SampleSlot> {
    let mut slots = vec![SampleSlot { name: "Layer" }; crate::devices::b10::multi::MAX_LAYERS];
    slots[0] = SampleSlot { name: "Sample" };
    slots
}

fn layout() -> Vec<Section> {
    use crate::sampler::p;
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Sample",
            items: vec![Item::Panel("sampler_tabs")],
        },
        Section {
            title: "Zones",
            items: vec![Item::Panel("sampler_zones")],
        },
        Section {
            title: "Output",
            items: vec![Item::Row(vec![
                knob(p::VOLUME),
                knob(p::PAN),
                knob(p::TUNE),
                knob(p::VELOCITY),
            ])],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.sampler",
    name: "Sampler",
    role: DeviceRole::Instrument,
    category: "Samplers",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::sampler::params(),
        layout: layout(),
        samples: slots(),
        ..Default::default()
    },
    create: |init| Box::new(crate::sampler::Sampler::with_init(init)),
};
