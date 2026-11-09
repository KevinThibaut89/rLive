//! Presets on disk, defaults, backups, and how the browser moves.

use rlive_browser::backup;
use rlive_browser::defaults::{Conversion, DefaultsLookup, DropTarget, UserLibrary};
use rlive_browser::item::{Item, ItemKey, ItemKind, Label, Role};
use rlive_browser::model::Move;
use rlive_browser::preset::{Payload, Preset, PresetError, TrackTemplate, PRESET_FORMAT};
use rlive_browser::query::{Results, Row, Scope};
use rlive_browser::tags::factory_tag;
use rlive_browser::Model;
use rlive_project::{
    Chain, Device, DeviceCompare, DeviceKind, FileRef, RackKind, Route, TrackKind,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rlive-preset-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compressor() -> Device {
    let mut d = Device::builtin("rlive.compressor", "Compressor");
    d.params.insert(1, -12.5);
    d.params.insert(2, 4.0);
    d.info = "My gentle squeeze".to_string();
    d
}

#[test]
fn a_device_preset_round_trips_through_a_file() {
    let temp = Temp::new("rt");
    let path = temp.0.join("p.rlpreset");
    let preset = Preset::of_device(&compressor(), "Squeeze");
    preset.save(&path).unwrap();
    let back = Preset::load(&path).unwrap();
    assert_eq!(back, preset);
    assert_eq!(back.format, PRESET_FORMAT);
    assert_eq!(back.info, "My gentle squeeze");
    assert_eq!(back.device_key().as_deref(), Some("rlive.compressor"));
    assert_eq!(back.role(), Some(Role::AudioEffect));
}

#[test]
fn a_rack_and_its_nested_devices_round_trip() {
    let temp = Temp::new("rack");
    let path = temp.0.join("rack.rlpreset");
    let mut rack = Device::rack(RackKind::Drum, "Kit");
    let mut chain = Chain::new("Kick");
    let mut sampler = Device::builtin("rlive.simpler", "Simpler");
    if let DeviceKind::Builtin { samples, .. } = &mut sampler.kind {
        samples.push(Some(FileRef::new("/tmp/kick.wav")));
    }
    chain.devices.push(sampler);
    if let DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(chain);
    }
    let preset = Preset::of_device(&rack, "Kit");
    preset.save(&path).unwrap();
    assert_eq!(Preset::load(&path).unwrap(), preset);
    assert_eq!(preset.device_key().as_deref(), Some("rack:Drum"));
    assert_eq!(preset.role(), Some(Role::Instrument));
}

#[test]
fn a_chain_and_a_track_default_round_trip() {
    let temp = Temp::new("chain");
    let mut chain = Chain::new("Lead");
    chain.devices.push(compressor());
    let preset = Preset::of_chain(&chain, "Lead Chain");
    preset.save(&temp.0.join("c.rlpreset")).unwrap();
    assert_eq!(Preset::load(&temp.0.join("c.rlpreset")).unwrap(), preset);

    let template = TrackTemplate {
        kind: TrackKind::Audio,
        volume_db: -6.0,
        pan: 0.25,
        sends: vec![-12.0],
        input: Route::None,
        output: Route::Default,
        devices: vec![compressor()],
    };
    let preset = Preset::of_track(template, "Default Audio Track");
    preset.save(&temp.0.join("t.rlpreset")).unwrap();
    assert_eq!(Preset::load(&temp.0.join("t.rlpreset")).unwrap(), preset);
}

#[test]
fn what_only_means_something_in_a_set_is_left_out_of_a_preset() {
    let mut d = compressor();
    d.sidechain = Some(rlive_project::SidechainSource {
        track: rlive_core::TrackId::new(),
        tap: rlive_project::Tap::default(),
    });
    d.compare = Some(Box::new(DeviceCompare::default()));
    let preset = Preset::of_device(&d, "X");
    let Payload::Device(saved) = &preset.payload else {
        panic!()
    };
    assert_eq!(saved.sidechain, None);
    assert_eq!(saved.compare, None, "the selected state is what's saved");
}

#[test]
fn format_one_presets_are_migrated() {
    let text = r#"(
        format: 1,
        name: "Old Reverb",
        type_id: "rlive.reverb",
        info: "From before the format had payloads",
        params: {1: 0.5, 3: 0.25},
    )"#;
    let preset = Preset::parse(text).unwrap();
    assert_eq!(preset.format, PRESET_FORMAT);
    assert_eq!(preset.name, "Old Reverb");
    let Payload::Device(d) = &preset.payload else {
        panic!("a device")
    };
    assert_eq!(d.params[&1], 0.5);
    assert!(matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.reverb"));
    // Saving it writes the current format.
    assert_eq!(Preset::parse(&preset.to_text()).unwrap(), preset);
}

#[test]
fn a_newer_or_broken_preset_is_refused() {
    assert!(matches!(
        Preset::parse("(format: 99, name: \"X\")"),
        Err(PresetError::TooNew(99))
    ));
    assert!(matches!(
        Preset::parse("not ron at all"),
        Err(PresetError::Parse(_))
    ));
}

// ------------------------------------------------------------- defaults

fn write_preset(path: &Path, name: &str) {
    Preset::of_device(&compressor(), name).save(path).unwrap();
}

#[test]
fn a_projects_defaults_win_over_the_user_librarys() {
    let temp = Temp::new("defaults");
    let lib = UserLibrary::new(temp.0.join("lib"));
    lib.ensure_folders().unwrap();
    let project = temp.0.join("Song Project");
    let in_library = lib.device_default_path(Role::AudioEffect, "Compressor");
    let lookup = DefaultsLookup::new(Some(lib.clone()), Some(&project));
    assert_eq!(lookup.device_preset(Role::AudioEffect, "Compressor"), None);
    write_preset(&in_library, "Compressor");
    assert_eq!(
        lookup
            .device_preset(Role::AudioEffect, "Compressor")
            .as_ref(),
        Some(&in_library)
    );
    let in_project = project
        .join("Defaults")
        .join("Audio Effects")
        .join("Compressor.rlpreset");
    write_preset(&in_project, "Compressor");
    assert_eq!(
        lookup
            .device_preset(Role::AudioEffect, "Compressor")
            .as_ref(),
        Some(&in_project),
        "the Project's default is the one used"
    );
    assert_eq!(
        lookup.device_preset(Role::Instrument, "Compressor"),
        None,
        "looked up by role"
    );
}

#[test]
fn a_default_has_to_carry_the_devices_own_name() {
    let temp = Temp::new("named");
    let lib = UserLibrary::new(temp.0.join("lib"));
    let path = lib
        .defaults_dir()
        .join("Audio Effects")
        .join("Squeezer.rlpreset");
    write_preset(&path, "Squeezer");
    let lookup = DefaultsLookup::new(Some(lib), None);
    assert_eq!(lookup.device_preset(Role::AudioEffect, "Compressor"), None);
}

fn set_time(path: &Path, seconds_ago: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(seconds_ago))
        .unwrap();
}

#[test]
fn the_last_added_sample_dropping_default_wins() {
    let temp = Temp::new("dropping");
    let lib = UserLibrary::new(temp.0.join("lib"));
    let dir = lib.dropping_dir(DropTarget::OnDrumRack);
    let (old, new) = (dir.join("A.rlpreset"), dir.join("Z.rlpreset"));
    write_preset(&old, "A");
    write_preset(&new, "Z");
    set_time(&old, 10);
    set_time(&new, 1000);
    let lookup = DefaultsLookup::new(Some(lib.clone()), None);
    assert_eq!(lookup.dropping(DropTarget::OnDrumRack).as_ref(), Some(&old));
    assert_eq!(lookup.dropping(DropTarget::OnDeviceView), None);
    let on_device = lib
        .dropping_dir(DropTarget::OnDeviceView)
        .join("B.rlpreset");
    write_preset(&on_device, "B");
    assert_eq!(
        lookup.dropping(DropTarget::OnDeviceView).as_ref(),
        Some(&on_device)
    );
}

#[test]
fn slicing_and_conversion_defaults_are_found_with_the_project_first() {
    let temp = Temp::new("slice");
    let lib = UserLibrary::new(temp.0.join("lib"));
    let project = temp.0.join("P");
    let lookup = DefaultsLookup::new(Some(lib.clone()), Some(&project));
    write_preset(&lib.slicing_dir().join("Kit.rlpreset"), "Kit");
    write_preset(&project.join("Defaults/Slicing/Mine.rlpreset"), "Mine");
    let slicing = lookup.slicing();
    assert_eq!(slicing.len(), 2);
    assert!(
        slicing[0].ends_with("Mine.rlpreset"),
        "the Project's is first"
    );

    let lib_melody = lib.audio_to_midi_dir(Conversion::Melody).join("M.rlpreset");
    write_preset(&lib_melody, "M");
    assert_eq!(
        lookup.audio_to_midi(Conversion::Melody).as_ref(),
        Some(&lib_melody)
    );
    let project_melody = project.join("Defaults/Audio to MIDI/Melody to MIDI/P.rlpreset");
    write_preset(&project_melody, "P");
    assert_eq!(
        lookup.audio_to_midi(Conversion::Melody).as_ref(),
        Some(&project_melody)
    );
    assert_eq!(lookup.audio_to_midi(Conversion::Harmony), None);
}

#[test]
fn track_defaults_are_found_by_kind() {
    let temp = Temp::new("track");
    let lib = UserLibrary::new(temp.0.join("lib"));
    let lookup = DefaultsLookup::new(Some(lib.clone()), None);
    let path = lib.track_default_path(TrackKind::Midi).unwrap();
    write_preset(&path, "Default MIDI Track");
    assert_eq!(lookup.track(TrackKind::Midi).as_ref(), Some(&path));
    assert_eq!(lookup.track(TrackKind::Audio), None);
    assert!(lib.track_default_path(TrackKind::Group).is_none());
}

#[test]
fn a_new_user_library_has_its_folders() {
    let temp = Temp::new("lib");
    let lib = UserLibrary::new(temp.0.join("lib"));
    lib.ensure_folders().unwrap();
    for f in [
        "Clips",
        "Defaults",
        "Grooves",
        "Presets",
        "Samples",
        "Templates",
    ] {
        assert!(lib.root.join(f).is_dir(), "{f}");
    }
}

// ------------------------------------------------------------- backups

#[test]
fn a_set_keeps_its_last_ten_saves_and_trashes_the_rest() {
    let temp = Temp::new("backup");
    let set = temp.0.join("Song.rlive");
    std::fs::write(&set, b"v1").unwrap();
    let trash = temp.0.join("Trash");
    let mut first = None;
    for n in 0..12u64 {
        std::fs::write(&set, format!("v{n}")).unwrap();
        let backup = backup::back_up(&set, 1_700_000_000 + n, Some(&trash)).unwrap();
        first.get_or_insert(backup);
    }
    let dir = backup::folder_of(&set).unwrap();
    let kept = backup::list(&dir, "Song");
    assert_eq!(kept.len(), backup::MAX_BACKUPS);
    assert!(
        kept[0].to_string_lossy().contains("[2023-11-14 2213"),
        "named by its time: {:?}",
        kept[0]
    );
    assert!(!first.unwrap().exists(), "the oldest went");
    assert_eq!(
        std::fs::read_dir(&trash).unwrap().count(),
        2,
        "to the trash"
    );
    assert_eq!(std::fs::read(&kept[9]).unwrap(), b"v11");
}

#[test]
fn stamps_are_real_dates() {
    assert_eq!(backup::stamp(0), "1970-01-01 000000");
    assert_eq!(backup::stamp(1_709_210_096), "2024-02-29 123456");
}

// ------------------------------------------------------------- navigation

fn rows(names: &[(&str, u8, bool, bool)]) -> Arc<Results> {
    Arc::new(Results {
        id: 1,
        rows: names
            .iter()
            .map(|(name, depth, has_children, expanded)| Row {
                item: Arc::new(Item::new(ItemKey((*name).into()), *name, ItemKind::Sample)),
                depth: *depth,
                has_children: *has_children,
                expanded: *expanded,
                colors: 0,
            })
            .collect(),
        search: false,
    })
}

fn k(s: &str) -> ItemKey {
    ItemKey(s.into())
}

fn model() -> Model {
    let mut m = Model::new();
    m.take_results(rows(&[
        ("a", 0, true, false),
        ("b", 0, false, false),
        ("c", 0, false, false),
    ]));
    m
}

#[test]
fn up_and_down_move_and_stop_at_the_ends() {
    let mut m = model();
    assert_eq!(m.move_cursor(1, false).unwrap().item.name, "a");
    assert_eq!(m.move_cursor(1, false).unwrap().item.name, "b");
    assert_eq!(m.move_cursor(-5, false).unwrap().item.name, "a");
    assert_eq!(m.move_cursor(9, false).unwrap().item.name, "c");
    assert_eq!(m.selected.len(), 1);
}

#[test]
fn shift_arrows_and_shift_clicks_extend_the_selection() {
    let mut m = model();
    m.click(k("a"), false, false);
    m.move_cursor(1, true);
    m.move_cursor(1, true);
    assert_eq!(m.selected.len(), 3);
    m.click(k("b"), false, false);
    m.click(k("c"), false, true);
    assert_eq!(m.selected_rows().len(), 2);
    m.click(k("a"), true, false);
    assert_eq!(m.selected_rows().len(), 3);
}

#[test]
fn right_opens_then_previews_and_left_closes_then_leaves() {
    let mut m = model();
    m.click(k("a"), false, false);
    assert_eq!(m.right(false), Move::Changed);
    assert!(m.expanded.contains(&k("a")));
    m.take_results(rows(&[
        ("a", 0, true, true),
        ("kid", 1, false, false),
        ("b", 0, false, false),
    ]));
    m.move_cursor(1, false);
    assert_eq!(
        m.right(false),
        Move::Preview,
        "nothing to open, so it plays"
    );
    assert_eq!(m.left(), Move::Changed, "up to the folder it's in");
    assert_eq!(m.cursor.as_ref(), Some(&k("a")));
    assert_eq!(m.left(), Move::Changed, "closes it");
    assert!(!m.expanded.contains(&k("a")));
    m.take_results(rows(&[("a", 0, true, false)]));
    assert_eq!(m.left(), Move::ToSidebar);
}

#[test]
fn opening_a_folder_closes_the_others_unless_told_not_to() {
    let mut m = Model::new();
    m.take_results(rows(&[("a", 0, true, true), ("b", 0, true, false)]));
    m.expanded.insert(k("a"));
    m.toggle_folder(&k("b"), true);
    assert!(m.expanded.contains(&k("a")) && m.expanded.contains(&k("b")));
    m.expanded.remove(&k("b"));
    m.toggle_folder(&k("b"), false);
    assert!(!m.expanded.contains(&k("a")));
    assert!(m.expanded.contains(&k("b")));
}

#[test]
fn history_goes_back_and_forward_through_searches_and_labels() {
    let mut m = Model::new();
    let samples = Scope::Label(Label::Samples);
    m.navigate(samples.clone());
    m.set_text("kick".into());
    m.commit_search();
    m.navigate(Scope::Label(Label::Clips));
    assert!(m.can_go_back());
    m.back();
    assert_eq!(m.nav.scope, samples);
    assert_eq!(m.nav.text, "kick");
    m.back();
    assert_eq!(m.nav.text, "");
    m.back();
    assert_eq!(m.nav.scope, Scope::Label(Label::All));
    assert!(!m.can_go_back());
    m.forward();
    m.forward();
    assert_eq!(m.nav.text, "kick");
    assert!(m.can_go_forward());
    m.navigate(Scope::Label(Label::Grooves));
    assert!(!m.can_go_forward(), "going somewhere new ends the forward");
}

#[test]
fn each_label_remembers_its_filters() {
    let mut m = Model::new();
    let (bass, keys) = (factory_tag("Bass"), factory_tag("Keys"));
    m.navigate(Scope::Label(Label::Sounds));
    m.click_tag(bass, false, &[bass, keys]);
    m.navigate(Scope::Label(Label::Samples));
    assert!(m.nav.tags.is_empty());
    m.navigate(Scope::Label(Label::Sounds));
    assert!(m.nav.tags.contains(&bass));
}

#[test]
fn clicking_a_tag_replaces_the_groups_choice_unless_cmd_is_held() {
    let mut m = Model::new();
    let (bass, keys, warm) = (
        factory_tag("Bass"),
        factory_tag("Keys"),
        factory_tag("Warm"),
    );
    let sounds = [bass, keys];
    m.click_tag(bass, false, &sounds);
    m.click_tag(warm, false, &[warm]);
    m.click_tag(keys, false, &sounds);
    assert!(!m.nav.tags.contains(&bass));
    assert!(m.nav.tags.contains(&keys) && m.nav.tags.contains(&warm));
    m.click_tag(bass, true, &sounds);
    assert!(m.nav.tags.contains(&bass) && m.nav.tags.contains(&keys));
    m.click_tag(keys, false, &sounds);
    assert!(!m.nav.tags.contains(&keys) && !m.nav.tags.contains(&bass));
}

#[test]
fn x_clears_words_and_clear_takes_the_tags_too() {
    let mut m = Model::new();
    let warm = factory_tag("Warm");
    m.set_text("pad".into());
    m.click_tag(warm, false, &[warm]);
    m.clear_text();
    assert_eq!(m.nav.text, "");
    assert!(m.nav.tags.contains(&warm), "x leaves the filters");
    m.set_text("pad".into());
    m.clear_all();
    assert_eq!(m.nav.text, "");
    assert!(m.nav.tags.is_empty());
}

#[test]
fn a_revealed_item_gets_the_cursor_when_its_row_arrives() {
    let mut m = Model::new();
    m.reveal(Scope::Label(Label::AudioEffects), [k("reverb")], k("plate"));
    assert!(m.expanded.contains(&k("reverb")));
    m.take_results(rows(&[("other", 0, false, false)]));
    assert_eq!(m.cursor, None);
    m.take_results(rows(&[
        ("reverb", 0, true, true),
        ("plate", 1, false, false),
    ]));
    assert_eq!(m.cursor, Some(k("plate")));
    assert!(m.selected.contains(&k("plate")));
}
