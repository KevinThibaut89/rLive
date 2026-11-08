//! Utility. Its implementation is in [`crate::effects`].

use crate::effects::util::*;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, ParamId, Section, Status};

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Channels",
            items: vec![
                Item::Menu(ParamId(CHANNEL_MODE)),
                toggle(INVERT_L),
                toggle(INVERT_R),
            ],
        },
        Section {
            title: "Stereo",
            items: vec![
                knob(WIDTH),
                knob(MID_SIDE),
                toggle(MID_SIDE_MODE),
                toggle(MONO),
            ],
        },
        Section {
            title: "Bass Mono",
            items: vec![toggle(BASS_MONO), knob(BASS_FREQ), toggle(BASS_AUDITION)],
        },
        Section {
            title: "Output",
            items: vec![knob(GAIN), knob(PAN), toggle(MUTE), toggle(DC)],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.utility",
    name: "Utility",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::utility_params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Utility>::default(),
};
