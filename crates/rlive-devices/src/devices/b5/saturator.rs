//! Saturator. Its implementation is in [`crate::effects::saturator`].

use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, ParamId, Section, Status};

fn layout() -> Vec<Section> {
    use crate::effects::sat::*;
    let knob = |id| Item::Knob(ParamId(id));
    vec![
        Section {
            title: "Shaper",
            items: vec![
                knob(DRIVE),
                Item::Menu(ParamId(CURVE)),
                knob(BASS_THRESHOLD),
                Item::Menu(ParamId(POST_CLIP)),
                knob(OUTPUT),
                knob(MIX),
            ],
        },
        Section {
            title: "Color",
            items: vec![
                Item::Toggle(ParamId(COLOR)),
                knob(AMT_LO),
                knob(AMT_HI),
                knob(FREQ),
                knob(WIDTH),
            ],
        },
        Section {
            title: "Waveshaper",
            items: vec![
                knob(WS_DRIVE),
                knob(WS_CURVE),
                knob(WS_DEPTH),
                knob(WS_LIN),
                knob(WS_DAMP),
                knob(WS_PERIOD),
            ],
        },
        Section {
            title: "Options",
            items: vec![
                Item::Toggle(ParamId(HI_QUALITY)),
                Item::Toggle(ParamId(PRE_DC)),
            ],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.saturator",
    name: "Saturator",
    role: DeviceRole::AudioEffect,
    category: "Distortion",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::saturator_params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Saturator>::default(),
};
