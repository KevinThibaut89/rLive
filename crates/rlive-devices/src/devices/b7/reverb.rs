//! Reverb. Its implementation is in [`crate::effects`], with the Studio engine alongside this.

use crate::effects::reverb as p;
use crate::{BuiltinInfo, DeviceRole};
use rlive_core::{DeviceDesc, Item, ParamId, Section, Status};

fn knob(id: u32) -> Item {
    Item::Knob(ParamId(id))
}

fn toggle(id: u32) -> Item {
    Item::Toggle(ParamId(id))
}

fn menu(id: u32) -> Item {
    Item::Menu(ParamId(id))
}

fn layout() -> Vec<Section> {
    vec![
        Section {
            title: "Input Filter",
            items: vec![
                toggle(p::INPUT_LOW_ON),
                toggle(p::INPUT_HIGH_ON),
                knob(p::INPUT_FREQ),
                knob(p::INPUT_WIDTH),
            ],
        },
        Section {
            title: "Early Reflections",
            items: vec![
                toggle(p::SPIN_ON),
                knob(p::SPIN_AMOUNT),
                knob(p::SPIN_RATE),
                knob(p::ER_SHAPE),
            ],
        },
        Section {
            title: "Diffusion Network",
            items: vec![
                toggle(p::HIGH_ON),
                knob(p::HIGH_FREQ),
                knob(p::HIGH_GAIN),
                toggle(p::LOW_ON),
                knob(p::LOW_FREQ),
                knob(p::LOW_GAIN),
                knob(p::DIFFUSION),
                knob(p::SCALE),
            ],
        },
        Section {
            title: "Chorus",
            items: vec![
                toggle(p::CHORUS_ON),
                knob(p::CHORUS_AMOUNT),
                knob(p::CHORUS_RATE),
            ],
        },
        Section {
            title: "Global",
            items: vec![
                menu(p::ENGINE),
                knob(p::PREDELAY),
                menu(p::SMOOTH),
                knob(p::SIZE),
                knob(p::DECAY),
                toggle(p::FREEZE),
                toggle(p::FLAT),
                toggle(p::CUT),
                knob(p::STEREO),
                menu(p::DENSITY),
            ],
        },
        Section {
            title: "Classic",
            items: vec![knob(p::DAMPING), knob(p::WIDTH)],
        },
        Section {
            title: "Output",
            items: vec![knob(p::REFLECT), knob(p::DIFFUSE), knob(p::MIX)],
        },
    ]
}

pub static INFO: BuiltinInfo = BuiltinInfo {
    id: "rlive.reverb",
    name: "Reverb",
    role: DeviceRole::AudioEffect,
    category: "Reverb & Resonance",
    status: Status::Beta,
    desc: || DeviceDesc {
        params: p::reverb_params(),
        layout: layout(),
        ..Default::default()
    },
    create: |_| Box::<crate::effects::Reverb>::default(),
};
