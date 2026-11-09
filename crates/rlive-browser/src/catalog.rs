//! The items that aren't files: built-in devices, plug-ins and factory presets.

use crate::factory;
use crate::item::{Item, ItemKey, ItemKind, Label, LabelSet, Role};
use crate::preset::{Payload, Preset};
use crate::scan::fill_preset;
use crate::tags::{factory_tag, TagId};
use rlive_devices::{BuiltinInfo, DeviceRole};

/// What a filename suggests is a drum sample.
pub const DRUM_WORDS: &[&str] = &[
    "kick", "snare", "clap", "hihat", "hi-hat", "hat ", "cymbal", "tom ", "perc", "drum",
];

/// The eighteen devices that ship as Max for Live devices in Live and are native here
/// (manual ch. 33, pp. 830-864).
pub const MAX_FOR_LIVE_DEVICES: [&str; 18] = [
    "rlive.ds_clang",
    "rlive.ds_clap",
    "rlive.ds_cymbal",
    "rlive.ds_fm",
    "rlive.ds_hh",
    "rlive.ds_kick",
    "rlive.ds_snare",
    "rlive.ds_tom",
    "rlive.align_delay",
    "rlive.envelope_follower",
    "rlive.lfo",
    "rlive.shaper",
    "rlive.envelope_midi",
    "rlive.expression_control",
    "rlive.midi_monitor",
    "rlive.mpe_control",
    "rlive.note_echo",
    "rlive.shaper_midi",
];

/// How a factory groove's or tuning's name is prefixed in its item's key.
pub const FACTORY_GROOVE: &str = "groove/";
pub const FACTORY_TUNING: &str = "tuning/";

/// A plug-in as the browser lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    pub format: String,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub is_instrument: bool,
    /// `|`-separated VST3 subcategories, empty for other formats.
    pub subcategories: String,
}

/// The Library labels a built-in device is listed under.
pub fn device_labels(type_id: &str) -> LabelSet {
    let Some(info) = rlive_devices::find(type_id) else {
        return LabelSet::default();
    };
    labels_of(info)
}

fn labels_of(info: &BuiltinInfo) -> LabelSet {
    let mut set = LabelSet::of(&[match info.role {
        DeviceRole::Instrument => Label::Instruments,
        DeviceRole::AudioEffect => Label::AudioEffects,
        DeviceRole::MidiEffect => Label::MidiEffects,
    }]);
    if info.category == "Modulators" {
        set.insert(Label::Modulators);
    }
    if info.category == "Drums" {
        set.insert(Label::Drums);
    }
    if MAX_FOR_LIVE_DEVICES.contains(&info.id) {
        set.insert(Label::MaxForLive);
    }
    set
}

fn role_of(info: &BuiltinInfo) -> Role {
    match info.role {
        DeviceRole::Instrument => Role::Instrument,
        DeviceRole::AudioEffect => Role::AudioEffect,
        DeviceRole::MidiEffect => Role::MidiEffect,
    }
}

/// The factory tag for a device category.
pub fn category_tag(category: &str) -> Option<TagId> {
    let name = match category {
        "Delay & Modulation" => "Delay & Modulation",
        "Distortion" => "Distortion",
        "Drums" => "Drum Machine",
        "Dynamics" => "Dynamics",
        "EQ & Filters" => "EQ & Filters",
        "MIDI Effects" => "MIDI Tool",
        "Modulators" => "Modulator",
        "Physical Models" => "Physical Model",
        "Reverb & Resonance" => "Reverb & Resonance",
        "Samplers" => "Sampler",
        "Spectral" => "Spectral",
        "Synths" => "Synthesizer",
        "Utility" => "Utility",
        _ => return None,
    };
    Some(factory_tag(name))
}

/// A built-in device, as a folder its presets open from.
pub fn builtin_item(info: &BuiltinInfo) -> Item {
    let mut item = Item::new(ItemKey::builtin(info.id), info.name, ItemKind::Device);
    item.device = Some(info.id.to_string());
    item.role = Some(role_of(info));
    item.labels = labels_of(info);
    item.factory_tags = category_tag(info.category).into_iter().collect();
    item.factory_tags.push(factory_tag("rLive"));
    item
}

/// Every device the browser offers, and the factory presets, parented to them.
pub fn builtin_items() -> Vec<Item> {
    let mut out: Vec<Item> = rlive_devices::builtins()
        .iter()
        .filter(|b| b.visible())
        .map(|b| builtin_item(b))
        .collect();
    for preset in factory::presets() {
        let mut item = Item::new(
            ItemKey::factory(&factory::key_of(&preset)),
            preset.name.clone(),
            ItemKind::Preset,
        );
        fill_preset(&mut item, &preset);
        item.factory_tags.push(factory_tag("rLive"));
        item.factory_tags.sort_unstable();
        item.factory_tags.dedup();
        out.push(item);
    }
    let asset = |key: String, name: &str, kind: ItemKind, label: Label| {
        let mut item = Item::new(ItemKey::factory(&key), name, kind);
        item.labels = LabelSet::of(&[label]);
        item.factory_tags = vec![factory_tag("rLive")];
        item
    };
    for g in rlive_project::factory_grooves() {
        let key = format!("{FACTORY_GROOVE}{}", g.name);
        out.push(asset(key, &g.name, ItemKind::Groove, Label::Grooves));
    }
    for t in rlive_project::factory_tunings() {
        let key = format!("{FACTORY_TUNING}{}", t.name);
        let mut item = asset(key, &t.name, ItemKind::Tuning, Label::Tunings);
        item.vendor = t.summary();
        out.push(item);
    }
    out
}

/// The tags a preset carries because of its device.
pub fn preset_device_tags(preset: &Preset) -> Vec<TagId> {
    let Payload::Device(d) = &preset.payload else {
        return Vec::new();
    };
    match &d.kind {
        rlive_project::DeviceKind::Builtin { type_id, .. } => rlive_devices::find(type_id)
            .and_then(|i| category_tag(i.category))
            .into_iter()
            .collect(),
        rlive_project::DeviceKind::Rack {
            kind: rlive_project::RackKind::Drum,
            ..
        } => vec![factory_tag("Drum Kit")],
        _ => Vec::new(),
    }
}

/// The factory tags a VST3 plug-in's subcategories map to (manual p. 75). Only subcategories that
/// map onto a tag of ours do.
pub fn vst3_tags(subcategories: &str) -> Vec<TagId> {
    let mut out = Vec::new();
    for sub in subcategories.split('|').map(str::trim) {
        let name = match sub {
            "Delay" | "Modulation" => "Delay & Modulation",
            "Distortion" => "Distortion",
            "Drum" => "Drum Machine",
            "Dynamics" => "Dynamics",
            "EQ" | "Filter" => "EQ & Filters",
            "Reverb" => "Reverb & Resonance",
            "Sampler" => "Sampler",
            "Spatial" | "Surround" | "Tools" | "Analyzer" | "Mastering" => "Utility",
            "Synth" => "Synthesizer",
            "Pitch Shift" | "Restoration" => "Spectral",
            _ => continue,
        };
        out.push(factory_tag(name));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The items for the Plug-Ins label: a folder per format, the plug-ins inside.
pub fn plugin_items(plugins: &[PluginEntry]) -> Vec<Item> {
    let mut out = Vec::new();
    let mut formats: Vec<&str> = plugins.iter().map(|p| p.format.as_str()).collect();
    formats.sort_unstable();
    formats.dedup();
    for format in formats {
        let mut folder = Item::new(
            ItemKey(format!("folder:plugins:{format}")),
            format,
            ItemKind::Folder,
        );
        folder.labels = LabelSet::of(&[Label::Plugins]);
        out.push(folder);
    }
    for p in plugins {
        let mut item = Item::new(
            ItemKey::plugin(&p.format, &p.id),
            p.name.clone(),
            ItemKind::Plugin,
        );
        item.vendor = p.vendor.clone();
        item.device = Some(format!("plugin:{}:{}", p.format, p.id));
        item.role = Some(if p.is_instrument {
            Role::Instrument
        } else {
            Role::AudioEffect
        });
        item.parent = Some(ItemKey(format!("folder:plugins:{}", p.format)));
        item.labels = LabelSet::of(&[Label::Plugins]);
        if p.is_instrument {
            item.labels.insert(Label::Instruments);
        } else {
            item.labels.insert(Label::AudioEffects);
        }
        item.factory_tags = vst3_tags(&p.subcategories);
        out.push(item);
    }
    out
}
