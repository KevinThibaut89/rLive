//! The browser driving a session, with no window.

use super::super::{BrowserItem, UiState};
use super::hot_swap::HotSwap;
use super::load::apply;
use super::*;
use crate::session::{ChainRef, Session, SwapTarget};
use rlive_browser::item::{Item, ItemKind, Label};
use rlive_browser::{Results, Row, Scope};
use rlive_engine::create_engine;
use rlive_plugin_host::scanner::PluginCache;
use rlive_project::{Device, Project, TrackKind};
use std::path::Path;
use std::sync::Arc;

fn session() -> (rlive_engine::Engine, Session) {
    let p = Project::new_default();
    let (engine, handle) = create_engine(48_000.0, 512);
    let s = Session::new(handle, p, PluginCache::default());
    (engine, s)
}

fn write_wav(p: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(p, spec).unwrap();
    for _ in 0..2_000 {
        w.write_sample(0i16).unwrap();
    }
    w.finalize().unwrap();
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rlive-browser-ui-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn state_for(s: &Session) -> UiState {
    UiState::initial(s, &crate::settings::Settings::default(), String::new())
}

fn device_row(id: &str, name: &str) -> Row {
    let mut item = Item::new(rlive_browser::ItemKey::builtin(id), name, ItemKind::Device);
    item.device = Some(id.to_string());
    Row {
        item: Arc::new(item),
        depth: 0,
        has_children: false,
        expanded: false,
        colors: 0,
    }
}

fn show_rows(state: &mut UiState, rows: Vec<Row>) {
    state.browser.model.take_results(Arc::new(Results {
        id: 1,
        rows,
        search: false,
    }));
}

#[test]
fn a_multi_selection_of_samples_goes_into_slots_or_a_scene() {
    let dir = temp("multi");
    let paths: Vec<PathBuf> = (0..3).map(|i| dir.join(format!("{i}.wav"))).collect();
    for p in &paths {
        write_wav(p);
    }
    let (_engine, mut s) = session();
    let audio = s.add_track(TrackKind::Audio, None);
    let audio2 = s.add_track(TrackKind::Audio, None);
    let mut state = state_for(&s);
    let items = BrowserItem::Many(paths.iter().cloned().map(BrowserItem::Sample).collect());

    drop_item(&mut s, &mut state, audio, &items, Some(0), false);
    let clips = s
        .project
        .track(audio)
        .unwrap()
        .clips
        .iter()
        .flatten()
        .count();
    assert_eq!(clips, 3, "one clip each, down the track");

    drop_item(&mut s, &mut state, audio2, &items, Some(4), true);
    let across: usize = [audio2]
        .iter()
        .map(|t| s.project.track(*t).unwrap().clips.iter().flatten().count())
        .sum();
    assert_eq!(
        across, 1,
        "Cmd puts one per track, as a scene, so only the audio tracks get one"
    );
}

#[test]
fn a_device_loads_on_the_selected_track_with_enter() {
    let (_engine, mut s) = session();
    let track = s.project.tracks[0].id;
    let mut state = state_for(&s);
    state.selected = Some(ChainRef::Track(track));
    show_rows(&mut state, vec![device_row("rlive.reverb", "Reverb")]);
    state.browser.model.move_cursor(1, false);
    let before = s.devices(ChainRef::Track(track)).len();
    apply_selection(&mut s, &mut state);
    assert_eq!(s.devices(ChainRef::Track(track)).len(), before + 1);
}

#[test]
fn a_sample_goes_in_a_simpler_on_a_midi_track_and_a_slot_on_an_audio_track() {
    let dir = temp("midi");
    let path = dir.join("hit.wav");
    write_wav(&path);
    let (_engine, mut s) = session();
    let midi = s.add_track(TrackKind::Midi, None);
    let audio = s.add_track(TrackKind::Audio, None);
    let mut state = state_for(&s);
    let item = BrowserItem::Sample(path);
    drop_item(&mut s, &mut state, midi, &item, None, false);
    assert!(s
        .devices(ChainRef::Track(midi))
        .iter()
        .any(|d| d.name == "Simpler" || d.name == "My Simpler"));
    drop_item(&mut s, &mut state, audio, &item, None, false);
    assert!(s.project.track(audio).unwrap().clips[0].is_some());
}

fn hot_swap_rig() -> (
    rlive_engine::Engine,
    Session,
    UiState,
    ChainRef,
    rlive_core::DeviceId,
) {
    let (engine, mut s) = session();
    let track = s.add_track(TrackKind::Audio, None);
    s.add_builtin(ChainRef::Track(track), "rlive.compressor");
    let chain = ChainRef::Track(track);
    let id = s.devices(chain)[0].id;
    let mut state = state_for(&s);
    state.selected = Some(chain);
    (engine, s, state, chain, id)
}

#[test]
fn q_enters_hot_swap_for_the_first_effect_and_reveals_it() {
    let (_engine, s, mut state, chain, id) = hot_swap_rig();
    toggle_hot_swap(&s, &mut state);
    assert_eq!(
        state.browser.hot_swap.unwrap().target,
        SwapTarget::Device { chain, id }
    );
    assert_eq!(
        state.browser.model.nav.scope,
        Scope::Label(Label::AudioEffects)
    );
    assert!(state
        .browser
        .model
        .expanded
        .contains(&rlive_browser::ItemKey::builtin("rlive.compressor")));
    toggle_hot_swap(&s, &mut state);
    assert!(state.browser.hot_swap.is_none(), "and Q again leaves");
}

#[test]
fn enter_in_hot_swap_replaces_the_target_instead_of_adding() {
    let (_engine, mut s, mut state, chain, _id) = hot_swap_rig();
    toggle_hot_swap(&s, &mut state);
    show_rows(&mut state, vec![device_row("rlive.reverb", "Reverb")]);
    state.browser.model.move_cursor(1, false);
    apply_selection(&mut s, &mut state);
    assert_eq!(s.devices(chain).len(), 1);
    assert_eq!(s.devices(chain)[0].name, "Reverb");
    assert!(
        state.browser.hot_swap.is_some(),
        "it stays on to audition more"
    );
}

#[test]
fn hot_swap_says_so_when_it_cannot_load_something() {
    let (_engine, mut s, mut state, chain, _) = hot_swap_rig();
    toggle_hot_swap(&s, &mut state);
    show_rows(&mut state, vec![device_row("rlive.poly", "Poly Synth")]);
    state.browser.model.move_cursor(1, false);
    apply_selection(&mut s, &mut state);
    assert_eq!(s.devices(chain)[0].name, "Compressor");
    assert!(!state.status.is_empty(), "and the status bar says why");
}

#[test]
fn d_flips_hot_swap_between_a_drum_rack_and_its_last_pad() {
    let (_engine, mut s) = session();
    let track = s.add_track(TrackKind::Midi, None);
    let chain = ChainRef::Track(track);
    let rack = Device::rack(rlive_project::RackKind::Drum, "Kit");
    let rack_id = rack.id;
    s.add_device(chain, Some(0), rack);
    let mut state = state_for(&s);
    state.selected = Some(chain);
    state.browser.hot_swap = Some(HotSwap {
        target: SwapTarget::Device { chain, id: rack_id },
        rack: Some((chain, rack_id, Some(40))),
    });
    toggle_drum_pad_target(&mut state);
    assert_eq!(
        state.browser.hot_swap.unwrap().target,
        SwapTarget::Pad {
            chain,
            rack: rack_id,
            note: 40,
        }
    );
    toggle_drum_pad_target(&mut state);
    assert_eq!(
        state.browser.hot_swap.unwrap().target,
        SwapTarget::Device { chain, id: rack_id }
    );
}

#[test]
fn a_project_device_can_be_copied_from_current_project() {
    let (_engine, mut s) = session();
    let a = s.add_track(TrackKind::Midi, None);
    let b = s.add_track(TrackKind::Audio, None);
    s.add_builtin(ChainRef::Track(b), "rlive.compressor");
    let source = s.devices(ChainRef::Track(b))[0].id;
    let mut state = state_for(&s);
    let before = s.devices(ChainRef::Track(a)).len();
    drop_item(
        &mut s,
        &mut state,
        a,
        &BrowserItem::ProjectDevice(source),
        None,
        false,
    );
    assert_eq!(s.devices(ChainRef::Track(a)).len(), before + 1);
    assert_ne!(s.devices(ChainRef::Track(a))[before].id, source);
}

#[test]
fn left_moves_to_the_sidebar_and_its_arrows_move_through_labels() {
    let (_engine, s) = session();
    let mut state = state_for(&s);
    show_rows(&mut state, vec![device_row("rlive.reverb", "Reverb")]);
    state.browser.model.move_cursor(1, false);
    assert_eq!(
        state.browser.model.left(),
        rlive_browser::model::Move::ToSidebar
    );
    assert_eq!(
        state.browser.model.pane,
        rlive_browser::model::Pane::Sidebar
    );
    state.browser.model.navigate(Scope::Label(Label::Samples));
    state.browser.sidebar_step(1);
    assert_eq!(state.browser.model.nav.scope, Scope::Label(Label::Grooves));
    state.browser.sidebar_step(-1);
    assert_eq!(state.browser.model.nav.scope, Scope::Label(Label::Samples));
}

#[test]
fn a_nested_chain_takes_what_is_loaded_when_it_is_selected() {
    let (_engine, mut s) = session();
    let track = s.add_track(TrackKind::Midi, None);
    let chain = ChainRef::Track(track);
    let mut rack = Device::rack(rlive_project::RackKind::Audio, "Rack");
    let inner = rlive_project::Chain::new("In");
    let inner_id = inner.id;
    if let rlive_project::DeviceKind::Rack { chains, .. } = &mut rack.kind {
        chains.push(inner);
    }
    let rack_id = rack.id;
    s.add_device(chain, None, rack);
    let nested = chain.child(rack_id, inner_id).unwrap();
    let mut state = state_for(&s);
    state.selected = Some(nested);
    apply(&mut s, &mut state, &BrowserItem::Builtin("rlive.utility"));
    assert_eq!(s.devices(nested).len(), 1);
}

fn asset_row(kind: ItemKind, key: &str, name: &str) -> Row {
    let item = Item::new(rlive_browser::ItemKey::factory(key), name, kind);
    Row {
        item: Arc::new(item),
        depth: 0,
        has_children: false,
        expanded: false,
        colors: 0,
    }
}

#[test]
fn enter_on_a_groove_puts_it_in_the_pool() {
    let (_engine, mut s) = session();
    let mut state = state_for(&s);
    let name = rlive_project::factory_grooves()[0].name.clone();
    let key = format!("groove/{name}");
    show_rows(&mut state, vec![asset_row(ItemKind::Groove, &key, &name)]);
    state.browser.model.move_cursor(1, false);
    apply_selection(&mut s, &mut state);
    assert_eq!(s.project.groove_pool.grooves.len(), 1);
    assert_eq!(s.project.groove_pool.grooves[0].name, name);
    assert!(state.show_groove_pool);
}

#[test]
fn hot_swapping_a_groove_replaces_the_one_the_clip_uses() {
    let (_engine, mut s) = session();
    let track = s.project.tracks[0].id;
    let slot = crate::session::ClipRef::slot(track, 0);
    let factory = rlive_project::factory_grooves();
    let first = s.add_groove(factory[0].clone());
    s.set_clip_groove(&[slot], Some(first));
    let mut state = state_for(&s);
    state.selected_clip = Some(slot);
    state.clip_view.tools.hot_swap = true;
    let key = format!("groove/{}", factory[1].name);
    show_rows(
        &mut state,
        vec![asset_row(ItemKind::Groove, &key, &factory[1].name)],
    );
    state.browser.model.move_cursor(1, false);
    apply_selection(&mut s, &mut state);
    assert_eq!(s.project.groove_pool.grooves.len(), 1, "swapped, not added");
    assert_eq!(s.project.groove_pool.grooves[0].name, factory[1].name);
    assert_eq!(
        s.clip_at(slot).unwrap().groove,
        Some(s.project.groove_pool.grooves[0].id),
        "and the clip follows"
    );
}

#[test]
fn dragging_a_groove_row_makes_one_pool_entry_however_often() {
    let (_engine, mut s) = session();
    let mut state = state_for(&s);
    let name = rlive_project::factory_grooves()[0].name.clone();
    let key = format!("groove/{name}");
    let row = asset_row(ItemKind::Groove, &key, &name);
    let id = super::load::pool_groove(&mut s, &mut state, &row).unwrap();
    assert_eq!(super::load::pool_groove(&mut s, &mut state, &row), Some(id));
    assert_eq!(s.project.groove_pool.grooves.len(), 1);
}

#[test]
fn enter_on_a_tuning_loads_it_and_opens_the_tuning_section() {
    let (_engine, mut s) = session();
    let mut state = state_for(&s);
    let name = rlive_project::factory_tunings()[0].name.clone();
    let key = format!("tuning/{name}");
    show_rows(&mut state, vec![asset_row(ItemKind::Tuning, &key, &name)]);
    state.browser.model.move_cursor(1, false);
    apply_selection(&mut s, &mut state);
    assert_eq!(s.tuning().map(|t| t.name.clone()), Some(name));
    assert!(state.show_tuning);
}

#[test]
fn a_tuning_asks_first_when_the_retune_option_is_on() {
    let (_engine, mut s) = session();
    let mut state = state_for(&s);
    state.retune_on_load = true;
    let name = rlive_project::factory_tunings()[0].name.clone();
    let track = s.project.tracks[0].id;
    drop_item(
        &mut s,
        &mut state,
        track,
        &BrowserItem::Tuning(AssetSource::Factory(name)),
        None,
        false,
    );
    assert!(s.tuning().is_none(), "not until it's confirmed");
    assert!(state.pending_tuning.is_some());
}

const CHORD_BANK: &str = r#"{"name":"Mine","chords":[{"name":"Fifth","intervals":[0,7]},
    {"name":"Octave","intervals":[0,12]}]}"#;
const CHORD_BANK_LEN: usize = 2;

#[test]
fn enter_on_a_chord_bank_replaces_the_stacks_bank() {
    use rlive_project::midi_tools::Generator;
    let dir = temp("stacks");
    let path = dir.join("Mine.stacks");
    std::fs::write(&path, CHORD_BANK).unwrap();
    let (_engine, mut s) = session();
    let mut state = state_for(&s);
    let track = s.project.tracks[0].id;
    drop_item(
        &mut s,
        &mut state,
        track,
        &BrowserItem::ChordBank(path),
        None,
        false,
    );
    assert!(state.status.is_empty(), "{}", state.status);
    let tools = &state.clip_view.midi_tools;
    let bank = tools
        .generators
        .iter()
        .find_map(|g| match g {
            Generator::Stacks(t) => Some(&t.bank),
            _ => None,
        })
        .unwrap();
    assert_eq!(bank.chords.len(), CHORD_BANK_LEN);
}
