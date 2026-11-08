//! EQ Three. Its implementation is in [`crate::effects`].

use crate::effects::eq::{self, *};
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, ParamId, Section, Status};

fn layout() -> Vec<Section> {
    let knob = |id| Item::Knob(ParamId(id));
    let toggle = |id| Item::Toggle(ParamId(id));
    vec![
        Section {
            title: "Low",
            items: vec![knob(eq::LOW_GAIN), toggle(LOW_ON), knob(LOW_FREQ)],
        },
        Section {
            title: "Mid",
            items: vec![knob(MID_GAIN), toggle(MID_ON)],
        },
        Section {
            title: "High",
            items: vec![knob(HIGH_GAIN), toggle(HIGH_ON), knob(HIGH_FREQ)],
        },
        Section {
            title: "Bands",
            items: vec![Item::Panel("eq_three_leds"), Item::Menu(ParamId(SLOPE))],
        },
        Section {
            title: "Classic",
            items: vec![knob(MID_FREQ), knob(MID_Q)],
        },
        Section {
            title: "Output",
            items: vec![knob(OUTPUT)],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.eq3",
    name: "EQ Three",
    role: DeviceRole::AudioEffect,
    category: "EQ & Filters",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: crate::effects::eq_params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Eq3>::default(),
};
