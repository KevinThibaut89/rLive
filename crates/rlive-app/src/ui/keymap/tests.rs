use super::super::commands::arrange::Arrange;
use super::super::commands::create::Create;
use super::super::commands::edit::Edit;
use super::super::commands::options::Options;
use super::super::commands::rack::Rack;
use super::super::commands::track::Track;
use super::super::commands::transport::Transport;
use super::super::commands::view::View;
use super::*;
use std::collections::BTreeMap;

const MAC: Platform = Platform::Mac;
const OTHER: Platform = Platform::Other;

const MANUAL: &str = include_str!("manual_shortcuts.tsv");

fn cx(focus: FocusArea) -> KeyContext {
    KeyContext {
        platform: OTHER,
        focus,
        clip_is_midi: true,
        computer_keys: false,
        text_focused: false,
        keyboard_taken: false,
        dialog_open: false,
        group_selected: false,
        warp_selected: false,
        chain_selected: false,
        time_selection: false,
        loop_brace: false,
        track_header: false,
        enter_held: false,
        slot_selected: false,
        lane_focused: false,
        notes_free: false,
        hot_swap: false,
        take_lane: false,
        take_header: false,
        take_area: false,
        tab_focus: false,
        renaming: false,
        value_control: false,
        track_title: false,
        scene_selected: false,
        in_track: false,
    }
}

fn cmd(key: Key) -> Chord {
    Chord {
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
        key,
    }
}

fn bare(key: Key) -> Chord {
    Chord {
        mods: Mods::default(),
        key,
    }
}

fn shift(key: Key) -> Chord {
    Chord {
        mods: Mods {
            shift: true,
            ..Mods::default()
        },
        key,
    }
}

fn mac_cmd() -> Modifiers {
    Modifiers::MAC_CMD | Modifiers::COMMAND
}

// ---------------------------------------------------------------- resolution by focus

#[test]
fn the_same_chord_means_different_things_by_focus() {
    let insert_scene = Resolution::Run(MenuCommand::Create(Create::NewScene));
    assert_eq!(resolve(cmd(Key::I), &cx(FocusArea::Session)), insert_scene);
    assert_eq!(
        resolve(cmd(Key::I), &cx(FocusArea::Arrangement)),
        Resolution::Run(MenuCommand::Arrange(Arrange::InsertSilence)),
        "Cmd+I inserts silence in the Arrangement"
    );
    assert_eq!(
        resolve(cmd(Key::E), &cx(FocusArea::Session)),
        Resolution::Run(MenuCommand::Launch(
            super::super::commands::launch::Launch::ToggleStopButton
        )),
    );
    let notes = cx(FocusArea::Clip);
    assert_eq!(
        resolve(cmd(Key::E), &notes),
        Resolution::Run(MenuCommand::Notes(
            super::super::commands::notes::Notes::ChopOrSplit
        ))
    );
    assert_eq!(
        resolve(cmd(Key::E), &cx(FocusArea::Browser)),
        Resolution::Unbound
    );
}

#[test]
fn the_sample_and_note_editors_are_separate_focus_areas() {
    let notes = cx(FocusArea::Clip);
    let sample = KeyContext {
        clip_is_midi: false,
        ..notes
    };
    // Cmd+J joins notes, and means nothing in the sample editor.
    assert_eq!(
        resolve(cmd(Key::J), &notes),
        Resolution::Run(MenuCommand::Notes(
            super::super::commands::notes::Notes::Join
        ))
    );
    assert_eq!(resolve(cmd(Key::J), &sample), Resolution::Unbound);
    // Cmd+I inserts a Warp Marker in the sample editor, which is a widget's job, so here it is
    // simply not a command; it means nothing for notes either.
    assert_eq!(resolve(cmd(Key::I), &sample), Resolution::Unbound);
    assert_eq!(resolve(cmd(Key::I), &notes), Resolution::Unbound);
}

#[test]
fn delete_goes_to_the_warp_marker_when_one_is_selected() {
    let nothing = KeyContext {
        clip_is_midi: false,
        ..cx(FocusArea::Clip)
    };
    let selected = KeyContext {
        warp_selected: true,
        ..nothing
    };
    assert_ne!(
        resolve(bare(Key::Delete), &selected),
        Resolution::Run(MenuCommand::Edit(Edit::Delete)),
        "the widget acts on the selection"
    );
    assert_eq!(
        resolve(bare(Key::Delete), &nothing),
        Resolution::Run(MenuCommand::Edit(Edit::Delete))
    );
}

#[test]
fn global_shortcuts_work_from_every_focus() {
    for focus in FocusArea::ALL {
        assert_eq!(
            resolve(cmd(Key::T), &cx(focus)),
            Resolution::Run(MenuCommand::Create(Create::NewAudioTrack)),
            "{focus:?}"
        );
        assert_eq!(
            resolve(bare(Key::Space), &cx(focus)),
            Resolution::Run(MenuCommand::Transport(Transport::PlayStop)),
            "{focus:?}"
        );
    }
}

#[test]
fn tab_switches_views_and_shift_tab_switches_the_detail() {
    let c = cx(FocusArea::Session);
    assert_eq!(
        resolve(bare(Key::Tab), &c),
        Resolution::Run(MenuCommand::View(View::ToggleMainView))
    );
    assert_eq!(
        resolve(shift(Key::Tab), &c),
        Resolution::Run(MenuCommand::View(View::SwitchDetail))
    );
    assert_eq!(
        resolve(bare(Key::F12), &c),
        Resolution::Run(MenuCommand::View(View::SwitchDetail))
    );
}

#[test]
fn moving_focus_commands_exist_for_each_built_view() {
    let alt = |key| Chord {
        mods: Mods {
            alt: true,
            ..Mods::default()
        },
        key,
    };
    let c = cx(FocusArea::Session);
    for (key, view) in [
        (Key::Num0, View::FocusControlBar),
        (Key::Num1, View::FocusSession),
        (Key::Num2, View::FocusArrangement),
        (Key::Num3, View::FocusClip),
        (Key::Num4, View::FocusDevice),
        (Key::Num5, View::FocusBrowser),
    ] {
        assert_eq!(
            resolve(alt(key), &c),
            Resolution::Run(MenuCommand::View(view))
        );
    }
    assert_eq!(
        resolve(alt(Key::Num6), &c),
        Resolution::Run(MenuCommand::View(View::FocusGroovePool))
    );
}

// ---------------------------------------------------------------- precedence

#[test]
fn text_fields_keep_their_keys() {
    let typing = KeyContext {
        text_focused: true,
        keyboard_taken: true,
        ..cx(FocusArea::Session)
    };
    assert_eq!(resolve(bare(Key::Space), &typing), Resolution::Unbound);
    assert_eq!(resolve(bare(Key::Tab), &typing), Resolution::Unbound);
    assert_eq!(resolve(cmd(Key::Z), &typing), Resolution::Unbound);
    assert_eq!(resolve(cmd(Key::A), &typing), Resolution::Unbound);
    assert_eq!(
        resolve(cmd(Key::S), &typing),
        Resolution::Run(MenuCommand::File(super::super::commands::file::File::Save))
    );
}

#[test]
fn a_focused_widget_keeps_bare_keys() {
    let focused = KeyContext {
        keyboard_taken: true,
        ..cx(FocusArea::Session)
    };
    assert_eq!(resolve(bare(Key::Space), &focused), Resolution::Unbound);
    assert_eq!(
        resolve(cmd(Key::Z), &focused),
        Resolution::Run(MenuCommand::Edit(Edit::Undo))
    );
}

#[test]
fn the_computer_keyboard_takes_its_letters_first() {
    let on = KeyContext {
        computer_keys: true,
        ..cx(FocusArea::Session)
    };
    assert_eq!(resolve(bare(Key::S), &on), Resolution::MidiKeyboard);
    assert_eq!(resolve(bare(Key::A), &on), Resolution::MidiKeyboard);
    assert_eq!(
        resolve(bare(Key::Z), &on),
        Resolution::Run(MenuCommand::Options(Options::OctaveDown))
    );
    assert_eq!(
        resolve(bare(Key::X), &on),
        Resolution::Run(MenuCommand::Options(Options::OctaveUp))
    );
    assert_eq!(
        resolve(bare(Key::C), &on),
        Resolution::Run(MenuCommand::Options(Options::VelocityDown))
    );
    assert_eq!(
        resolve(bare(Key::V), &on),
        Resolution::Run(MenuCommand::Options(Options::VelocityUp))
    );
}

#[test]
fn shift_reaches_letter_shortcuts_under_the_computer_keyboard() {
    let on = KeyContext {
        computer_keys: true,
        ..cx(FocusArea::Session)
    };
    assert_eq!(
        resolve(shift(Key::S), &on),
        Resolution::Run(MenuCommand::Track(Track::Solo))
    );
    assert_eq!(
        resolve(shift(Key::C), &on),
        Resolution::Run(MenuCommand::Track(Track::Arm))
    );
}

#[test]
fn plus_and_minus_fold_a_selected_group_and_zoom_otherwise() {
    let group = KeyContext {
        group_selected: true,
        ..cx(FocusArea::Arrangement)
    };
    assert_eq!(
        resolve(bare(Key::Plus), &group),
        Resolution::Run(MenuCommand::Track(Track::ShowGroupedTracks))
    );
    assert_eq!(
        resolve(bare(Key::Minus), &group),
        Resolution::Run(MenuCommand::Track(Track::HideGroupedTracks))
    );
    assert_eq!(
        resolve(bare(Key::U), &group),
        Resolution::Run(MenuCommand::Track(Track::ToggleGroupFold))
    );
    assert_eq!(
        resolve(bare(Key::Plus), &cx(FocusArea::Arrangement)),
        Resolution::Run(MenuCommand::View(View::RulerZoomIn))
    );
}

#[test]
fn cmd_g_groups_tracks_and_groups_notes_by_focus() {
    let group = Resolution::Run(MenuCommand::Create(Create::GroupTracks));
    assert_eq!(resolve(cmd(Key::G), &cx(FocusArea::Session)), group);
    assert_eq!(resolve(cmd(Key::G), &cx(FocusArea::Arrangement)), group);
    assert_eq!(
        resolve(cmd(Key::G), &cx(FocusArea::Clip)),
        Resolution::Run(MenuCommand::Notes(
            super::super::commands::notes::Notes::Group
        ))
    );
    assert_eq!(
        resolve(cmd(Key::G), &cx(FocusArea::Device)),
        Resolution::Run(MenuCommand::Rack(Rack::Group))
    );
}

#[test]
fn cmd_shift_g_ungroups_a_rack_with_the_device_view_focused() {
    let shift_cmd = Chord {
        mods: Mods {
            cmd: true,
            shift: true,
            ..Default::default()
        },
        key: Key::G,
    };
    assert_eq!(
        resolve(shift_cmd, &cx(FocusArea::Device)),
        Resolution::Run(MenuCommand::Rack(Rack::Ungroup))
    );
}

#[test]
fn s_solos_a_chain_selected_in_the_mixer() {
    let mut c = cx(FocusArea::Session);
    assert_eq!(
        resolve(bare(Key::S), &c),
        Resolution::Run(MenuCommand::Track(Track::Solo)),
        "with no chain selected it is the track's"
    );
    c.chain_selected = true;
    assert_eq!(
        resolve(bare(Key::S), &c),
        Resolution::Run(MenuCommand::Rack(Rack::SoloChain))
    );
}

#[test]
fn escape_closes_a_dialog_only_when_one_is_open() {
    let closed = cx(FocusArea::Session);
    assert_eq!(resolve(bare(Key::Escape), &closed), Resolution::Unbound);
    let open = KeyContext {
        dialog_open: true,
        ..closed
    };
    assert_eq!(
        resolve(bare(Key::Escape), &open),
        Resolution::Run(MenuCommand::View(View::CloseDialog))
    );
}

#[test]
fn m_toggles_the_keyboard_in_both_states_and_keys_it_does_not_use_still_work() {
    let on = KeyContext {
        computer_keys: true,
        ..cx(FocusArea::Session)
    };
    let off = cx(FocusArea::Session);
    let toggle = Resolution::Run(MenuCommand::Options(Options::ToggleComputerKeys));
    assert_eq!(resolve(bare(Key::M), &on), toggle);
    assert_eq!(resolve(bare(Key::M), &off), toggle);
    assert_eq!(
        resolve(bare(Key::O), &on),
        Resolution::Run(MenuCommand::Transport(Transport::ToggleMetronome))
    );
}

#[test]
fn with_the_keyboard_off_its_letters_are_ordinary_shortcuts() {
    let off = cx(FocusArea::Arrangement);
    assert_eq!(
        resolve(bare(Key::S), &off),
        Resolution::Run(MenuCommand::Track(Track::Solo))
    );
    assert_eq!(
        resolve(bare(Key::Z), &off),
        Resolution::Run(MenuCommand::Arrange(Arrange::ZoomToSelection))
    );
    assert_eq!(
        resolve(bare(Key::C), &off),
        Resolution::Run(MenuCommand::Track(Track::Arm))
    );
}

// ---------------------------------------------------------------- keyboard layouts

#[test]
fn letters_follow_what_the_key_types_not_where_it_is() {
    // AZERTY: the key in the QWERTY Z position types W, and the QWERTY W position types Z.
    let undo_on_azerty = chord_of(Key::Z, Some(Key::W), mac_cmd(), MAC);
    assert_eq!(undo_on_azerty, cmd(Key::Z));
    let not_undo = chord_of(Key::W, Some(Key::Z), mac_cmd(), MAC);
    assert_eq!(not_undo, cmd(Key::W));
    // The physical A key types Q on AZERTY.
    let q = chord_of(Key::Q, Some(Key::A), Modifiers::NONE, OTHER);
    assert_eq!(q, bare(Key::Q));
}

#[test]
fn the_digit_row_works_when_it_types_symbols() {
    // AZERTY: Cmd+[physical 4] reports the apostrophe, which egui names Quote.
    let c = chord_of(Key::Quote, Some(Key::Num4), mac_cmd(), MAC);
    assert_eq!(c, cmd(Key::Num4));
    // Cmd+[physical 8] reports an exclamation mark.
    let c = chord_of(Key::Exclamationmark, Some(Key::Num8), mac_cmd(), MAC);
    assert_eq!(c, cmd(Key::Num8));
    assert_eq!(
        resolve(c, &cx(FocusArea::Session)),
        Resolution::Run(MenuCommand::Transport(Transport::QuantizeQuarter))
    );
}

#[test]
fn digits_typed_with_shift_on_azerty_are_plain_digits() {
    let c = chord_of(Key::Num0, Some(Key::Num0), Modifiers::SHIFT, OTHER);
    assert_eq!(c, bare(Key::Num0));
    assert_eq!(
        resolve(c, &cx(FocusArea::Session)),
        Resolution::Run(MenuCommand::Track(Track::ToggleActivator))
    );
    // Alt+Shift+1 keeps its Shift: there it's a chord of its own.
    let m = Modifiers::SHIFT | Modifiers::ALT;
    let c = chord_of(Key::Num1, Some(Key::Num1), m, OTHER);
    assert!(c.mods.shift && c.mods.alt);
}

#[test]
fn shifted_digit_row_symbols_are_not_digits_on_qwerty() {
    // QWERTY Shift+1 types "!": not a shortcut for the digit 1.
    let c = chord_of(
        Key::Exclamationmark,
        Some(Key::Num1),
        Modifiers::SHIFT,
        OTHER,
    );
    assert_eq!(c, shift(Key::Num1));
    assert_eq!(resolve(c, &cx(FocusArea::Browser)), Resolution::Unbound);
}

#[test]
fn symbols_are_matched_however_the_layout_types_them() {
    // "?" is Shift+, on AZERTY and Shift+/ on QWERTY.
    let info = Resolution::Run(MenuCommand::View(View::ToggleInfoView));
    for chord in [
        chord_of(Key::Questionmark, Some(Key::M), Modifiers::SHIFT, OTHER),
        chord_of(Key::Questionmark, Some(Key::Slash), Modifiers::SHIFT, OTHER),
    ] {
        assert_eq!(resolve(chord, &cx(FocusArea::Session)), info);
    }
    // Cmd + "+" reports "=" with Shift on most layouts.
    let zoom = chord_of(
        Key::Equals,
        Some(Key::Equals),
        mac_cmd() | Modifiers::SHIFT,
        MAC,
    );
    assert_eq!(zoom, cmd(Key::Equals));
    let mac = KeyContext {
        platform: MAC,
        ..cx(FocusArea::Arrangement)
    };
    assert_eq!(
        resolve(zoom, &mac),
        Resolution::Run(MenuCommand::View(View::ZoomIn))
    );
    let plus = chord_of(Key::Plus, Some(Key::Equals), Modifiers::SHIFT, OTHER);
    assert_eq!(
        resolve(plus, &cx(FocusArea::Arrangement)),
        Resolution::Run(MenuCommand::View(View::RulerZoomIn))
    );
}

#[test]
fn mac_delete_is_backspace() {
    let c = chord_of(Key::Backspace, Some(Key::Backspace), Modifiers::NONE, MAC);
    assert_eq!(
        resolve(
            c,
            &KeyContext {
                platform: MAC,
                ..cx(FocusArea::Session)
            }
        ),
        Resolution::Run(MenuCommand::Edit(Edit::Delete))
    );
}

// ---------------------------------------------------------------- native menu

#[test]
fn the_native_menu_owns_its_chords_so_they_never_fire_twice() {
    let mac = KeyContext {
        platform: MAC,
        ..cx(FocusArea::Session)
    };
    for chord in NATIVE_CHORDS.iter() {
        assert_eq!(
            resolve(*chord, &mac),
            Resolution::Unbound,
            "{}",
            chord_text(*chord, MAC)
        );
    }
    assert!(NATIVE_CHORDS.contains(&cmd(Key::Z)));
    assert!(!NATIVE_CHORDS.contains(&bare(Key::Tab)));
    // Elsewhere the keymap fires them itself.
    assert_eq!(
        resolve(cmd(Key::Z), &cx(FocusArea::Session)),
        Resolution::Run(MenuCommand::Edit(Edit::Undo))
    );
}

#[test]
fn every_command_in_a_menu_with_a_cmd_chord_shows_it() {
    assert_eq!(
        menu_chord(MenuCommand::Edit(Edit::Undo), MAC),
        Some(cmd(Key::Z))
    );
    assert_eq!(menu_chord(MenuCommand::Edit(Edit::Delete), MAC), None);
    assert_eq!(
        menu_chord(MenuCommand::Create(Create::NewAudioTrack), MAC),
        Some(cmd(Key::T))
    );
}

#[test]
fn process_takes_shortcuts_and_leaves_notes() {
    let press = |key, physical| Event::Key {
        key,
        physical_key: Some(physical),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let release = |key| Event::Key {
        key,
        physical_key: Some(key),
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let mut raw = RawInput::default();
    raw.events.push(press(Key::Space, Key::Space));
    raw.events.push(press(Key::Space, Key::Space)); // a repeat
    raw.events.push(release(Key::Space));
    raw.events.push(press(Key::A, Key::Q));
    let c = KeyContext {
        computer_keys: true,
        ..cx(FocusArea::Session)
    };
    let mut state = KeyState::default();
    let mut commands = Vec::new();
    state.process(&mut raw, &c, &mut commands);
    assert_eq!(commands, vec![MenuCommand::Transport(Transport::PlayStop)]);
    assert_eq!(raw.events.len(), 1, "only the note press stays");
}

// ---------------------------------------------------------------- the table itself

fn focus_contexts() -> Vec<KeyContext> {
    let mut out = Vec::new();
    for focus in FocusArea::ALL {
        for clip_is_midi in [false, true] {
            for computer_keys in [false, true] {
                let c = KeyContext {
                    clip_is_midi,
                    computer_keys,
                    ..cx(focus)
                };
                out.push(c);
            }
        }
    }
    out
}

#[test]
fn no_two_bindings_fight_over_a_chord() {
    // Windows uses Ctrl+Shift+F for both of these in the note editor; the manual lists both.
    let known = |platform: Platform, b: &Binding, chord: Chord| {
        platform == OTHER
            && chord
                == Chord {
                    mods: Mods {
                        cmd: true,
                        shift: true,
                        ..Mods::default()
                    },
                    key: Key::F,
                }
            && b.area == Area::ClipNotes
    };
    for platform in [MAC, OTHER] {
        let mut groups: HashMap<(Chord, Area, When), Vec<&Binding>> = HashMap::new();
        for b in BINDINGS {
            if let Some(c) = b.trigger.chord(platform) {
                groups
                    .entry((normalized(c), b.area, b.when))
                    .or_default()
                    .push(b);
            }
        }
        for ((chord, area, when), bs) in groups {
            let same = bs.iter().all(|b| b.status == bs[0].status);
            assert!(
                same || bs.iter().all(|b| known(platform, b, chord)),
                "{} in {area:?}/{when:?}: {:?}",
                chord_text(chord, platform),
                bs.iter().map(|b| b.label).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn a_pending_binding_never_hides_a_live_one() {
    for platform in [MAC, OTHER] {
        for (&(p, chord), bindings) in BY_CHORD.iter() {
            if p != platform {
                continue;
            }
            for c in focus_contexts() {
                let c = KeyContext { platform, ..c };
                let live = bindings.iter().any(|b| {
                    matches!(b.status, Status::Live(Exec::Command(_)))
                        && b.area.contains(c.focus, c.clip_is_midi)
                        && c.holds(b.when)
                });
                if !live {
                    continue;
                }
                let found = lookup(chord, &c).unwrap();
                assert!(
                    !matches!(found.status, Status::Pending(_)),
                    "{} in {:?}: pending '{}' hides a working shortcut",
                    chord_text(chord, platform),
                    c.focus,
                    found.label
                );
            }
        }
    }
}

#[test]
fn every_live_command_binding_runs_its_command() {
    for b in BINDINGS {
        let Status::Live(Exec::Command(c)) = b.status else {
            continue;
        };
        assert!(MenuCommand::all().contains(&c), "{}", b.label);
    }
}

/// Canonical text of a chord list in the manual fixture's own notation.
fn fixture_chords(cell: &str) -> Vec<String> {
    cell.split('|')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from)
        .collect()
}

fn binding_text(b: &Binding, platform: Platform) -> Option<String> {
    match b.trigger {
        Trigger::Gesture { mac, other } => {
            Some(format!("~{}", if platform == MAC { mac } else { other }))
        }
        t => t.chord(platform).map(|c| chord_text(c, platform)),
    }
}

/// How many bindings in each manual section use each chord.
fn table_counts(platform: Platform) -> BTreeMap<(String, String), usize> {
    let mut counts = BTreeMap::new();
    for b in BINDINGS.iter().filter(|b| b.section != "rlive") {
        if let Some(text) = binding_text(b, platform) {
            *counts.entry((b.section.to_string(), text)).or_insert(0) += 1;
        }
    }
    counts
}

#[test]
fn every_shortcut_in_the_manual_is_declared() {
    let mut problems = Vec::new();
    for (platform, column) in [(OTHER, 1), (MAC, 2)] {
        let counts = table_counts(platform);
        let mut wanted: BTreeMap<(String, String), usize> = BTreeMap::new();
        for line in MANUAL.lines().filter(|l| !l.starts_with('#')) {
            let cells: Vec<&str> = line.split('\t').collect();
            for chord in fixture_chords(cells[column]) {
                *wanted.entry((cells[0].to_string(), chord)).or_insert(0) += 1;
            }
        }
        for ((section, chord), n) in &wanted {
            let have = counts
                .get(&(section.clone(), chord.clone()))
                .copied()
                .unwrap_or(0);
            if have < *n {
                problems.push(format!("{platform:?} {section}: {chord} ({have}/{n})"));
            }
        }
        for ((section, chord), n) in &counts {
            let want = wanted
                .get(&(section.clone(), chord.clone()))
                .copied()
                .unwrap_or(0);
            if *n > want {
                problems.push(format!(
                    "{platform:?} {section}: extra {chord} ({n}, manual {want})"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_manual_section_has_bindings() {
    let sections: std::collections::HashSet<&str> = BINDINGS.iter().map(|b| b.section).collect();
    for n in 1..=24 {
        let s = format!("42.{n}");
        assert!(sections.contains(s.as_str()), "{s} has no bindings");
    }
}

// ---------------------------------------------------------------- the checklist

/// The checklist's generated block listing pending shortcuts.
fn pending_block() -> String {
    let mut by_milestone: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for b in BINDINGS {
        if let Status::Pending(m) = b.status {
            by_milestone.entry(m).or_default().push(b.label.to_string());
        }
    }
    let mut out = String::new();
    for (milestone, labels) in by_milestone {
        let mut unique: Vec<&str> = labels.iter().map(String::as_str).collect();
        unique.sort_unstable();
        unique.dedup();
        out.push_str(&format!("- `[ ]` **{milestone}:** {}\n", unique.join("; ")));
    }
    out
}

#[test]
fn pending_shortcuts_are_listed_in_the_checklist() {
    const BEGIN: &str = "<!-- pending-shortcuts:begin -->\n";
    const END: &str = "<!-- pending-shortcuts:end -->\n";
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/live12-parity.md");
    let doc = std::fs::read_to_string(path).unwrap();
    let start = doc.find(BEGIN).expect("begin marker") + BEGIN.len();
    let end = doc.find(END).expect("end marker");
    let expected = pending_block();
    if std::env::var_os("RLIVE_UPDATE_CHECKLIST").is_some() {
        let updated = format!("{}{expected}{}", &doc[..start], &doc[end..]);
        std::fs::write(path, updated).unwrap();
        return;
    }
    assert_eq!(
        &doc[start..end],
        expected,
        "checklist is stale; run with RLIVE_UPDATE_CHECKLIST=1"
    );
}

// ---------------------------------------------------------------- the Arrangement's selection

#[test]
fn take_lane_keys_go_to_the_take_lane_only_while_one_is_selected() {
    use super::super::commands::comping::Comp;
    let arr = cx(FocusArea::Arrangement);
    let run = |c: KeyContext, ch: Chord| resolve(ch, &c);
    // Enter belongs to Add Take Area only with a take lane in hand.
    assert!(!matches!(
        run(arr, bare(Key::Enter)),
        Resolution::Run(MenuCommand::Comp(_))
    ));
    let on_lane = KeyContext {
        take_lane: true,
        ..arr
    };
    assert_eq!(
        run(on_lane, bare(Key::Enter)),
        Resolution::Run(MenuCommand::Comp(Comp::CopyToMain))
    );
    assert_eq!(
        run(on_lane, bare(Key::T)),
        Resolution::Run(MenuCommand::Comp(Comp::Audition))
    );
    // Cmd+Up and Cmd+Down step through takes on a selection, and reorder a lane in its header.
    let area = KeyContext {
        take_area: true,
        ..arr
    };
    assert_eq!(
        run(area, cmd(Key::ArrowUp)),
        Resolution::Run(MenuCommand::Comp(Comp::NextTake))
    );
    let header = KeyContext {
        take_header: true,
        take_lane: true,
        ..arr
    };
    assert_eq!(
        run(header, cmd(Key::ArrowUp)),
        Resolution::Run(MenuCommand::Comp(Comp::MoveLaneUp))
    );
    assert_eq!(
        run(header, cmd(Key::D)),
        Resolution::Run(MenuCommand::Comp(Comp::DuplicateLane))
    );
    assert_eq!(
        run(header, bare(Key::ArrowLeft)),
        Resolution::Run(MenuCommand::Comp(Comp::FoldToMain))
    );
    // Off a header, Cmd+D is Duplicate as ever.
    assert_eq!(
        run(area, cmd(Key::D)),
        Resolution::Run(MenuCommand::Edit(Edit::Duplicate))
    );
}

#[test]
fn arrows_act_on_what_the_arrangement_has_selected() {
    let plain = cx(FocusArea::Arrangement);
    assert_eq!(
        resolve(bare(Key::ArrowLeft), &plain),
        Resolution::Run(MenuCommand::Arrange(Arrange::MoveLeft))
    );
    let selection = KeyContext {
        time_selection: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::ArrowLeft), &selection),
        Resolution::Run(MenuCommand::Arrange(Arrange::NudgeLeft))
    );
    let brace = KeyContext {
        loop_brace: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::ArrowLeft), &brace),
        Resolution::Run(MenuCommand::Arrange(Arrange::LoopNudgeLeft))
    );
    assert_eq!(
        resolve(cmd(Key::ArrowUp), &brace),
        Resolution::Run(MenuCommand::Arrange(Arrange::LoopDouble))
    );
    let lanes = KeyContext {
        lane_focused: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::ArrowLeft), &lanes),
        Resolution::Unbound,
        "an automation lane has the arrows"
    );
    let enter = KeyContext {
        enter_held: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::ArrowRight), &enter),
        Resolution::Run(MenuCommand::Arrange(Arrange::ResizeRight))
    );
}

#[test]
fn the_arrow_keys_go_to_the_loop_brace_when_it_is_selected() {
    use crate::ui::commands::clip::Clip as Cv;
    let nothing = cx(FocusArea::Clip);
    let selected = KeyContext {
        loop_brace: true,
        ..nothing
    };
    assert_ne!(
        resolve(bare(Key::ArrowLeft), &nothing),
        Resolution::Run(MenuCommand::Clip(Cv::NudgeLeft))
    );
    assert_eq!(
        resolve(bare(Key::ArrowLeft), &selected),
        Resolution::Run(MenuCommand::Clip(Cv::NudgeLeft))
    );
    assert_eq!(
        resolve(cmd(Key::ArrowUp), &selected),
        Resolution::Run(MenuCommand::Clip(Cv::DoubleLoop))
    );
}

#[test]
fn zero_deactivates_the_selection_or_the_track() {
    let plain = cx(FocusArea::Arrangement);
    assert_eq!(
        resolve(bare(Key::Num0), &plain),
        Resolution::Run(MenuCommand::Track(Track::ToggleActivator))
    );
    let selection = KeyContext {
        time_selection: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::Num0), &selection),
        Resolution::Run(MenuCommand::Arrange(Arrange::Deactivate))
    );
}

#[test]
fn the_global_quantization_shortcuts_are_live() {
    use crate::ui::commands::transport::Transport as Tr;
    let c = cx(FocusArea::Clip);
    for (key, want) in [
        (Key::Num6, Tr::QuantizeSixteenth),
        (Key::Num7, Tr::QuantizeEighth),
        (Key::Num8, Tr::QuantizeQuarter),
        (Key::Num9, Tr::QuantizeBar),
        (Key::Num0, Tr::QuantizeOff),
    ] {
        assert_eq!(
            resolve(cmd(key), &c),
            Resolution::Run(MenuCommand::Transport(want))
        );
    }
}

#[test]
fn the_tab_cycle_is_a_clip_view_shortcut() {
    use crate::ui::commands::clip::Clip as Cv;
    let c = KeyContext {
        platform: MAC,
        ..cx(FocusArea::Clip)
    };
    let alt_tab = Chord {
        mods: Mods {
            alt: true,
            ..Mods::default()
        },
        key: Key::Tab,
    };
    assert_eq!(
        resolve(alt_tab, &c),
        Resolution::Run(MenuCommand::Clip(Cv::CycleEditorMode))
    );
}

#[test]
fn ctrl_shift_f_opens_the_note_filters_on_windows_and_follow_stays_elsewhere() {
    let notes = KeyContext {
        platform: Platform::Other,
        ..cx(FocusArea::Clip)
    };
    let chord = Chord {
        mods: Mods {
            cmd: true,
            shift: true,
            ..Mods::default()
        },
        key: Key::F,
    };
    assert_eq!(
        resolve(chord, &notes),
        Resolution::Run(MenuCommand::Notes(
            super::super::commands::notes::Notes::ToggleFilters
        ))
    );
    let sample = KeyContext {
        clip_is_midi: false,
        ..notes
    };
    assert_eq!(
        resolve(chord, &sample),
        Resolution::Run(MenuCommand::Clip(
            super::super::commands::clip::Clip::ToggleFollow
        )),
    );
}

#[test]
fn the_note_editor_arrows_yield_to_a_selected_marker() {
    let free = KeyContext {
        notes_free: true,
        ..cx(FocusArea::Clip)
    };
    let shift_left = Chord {
        mods: Mods {
            shift: true,
            ..Mods::default()
        },
        key: Key::ArrowLeft,
    };
    assert_eq!(
        resolve(shift_left, &free),
        Resolution::Run(MenuCommand::Notes(
            super::super::commands::notes::Notes::ExtendLeft
        ))
    );
    let marker = KeyContext {
        notes_free: false,
        ..cx(FocusArea::Clip)
    };
    assert_eq!(
        resolve(shift_left, &marker),
        Resolution::Run(MenuCommand::Clip(
            super::super::commands::clip::Clip::ShiftRegionLeft
        ))
    );
}

// ---------------------------------------------------------------- C17 and C19

fn run(c: impl Into<MenuCommand>) -> Resolution {
    Resolution::Run(c.into())
}

fn cmd_alt(key: Key) -> Chord {
    Chord {
        mods: Mods {
            cmd: true,
            alt: true,
            ..Mods::default()
        },
        key,
    }
}

#[test]
fn the_second_window_sends_and_returns_are_global_view_commands() {
    use super::super::commands::view::View;
    for focus in FocusArea::ALL {
        let c = cx(focus);
        let cmd_shift_w = Chord {
            mods: Mods {
                cmd: true,
                shift: true,
                ..Mods::default()
            },
            key: Key::W,
        };
        assert_eq!(resolve(cmd_shift_w, &c), run(View::ToggleSecondWindow));
        assert_eq!(resolve(cmd_alt(Key::S), &c), run(View::ToggleSends));
        assert_eq!(resolve(cmd_alt(Key::R), &c), run(View::ToggleReturns));
    }
}

#[test]
fn f12_still_flips_the_detail_view_from_every_focus() {
    use super::super::commands::view::View;
    for focus in FocusArea::ALL {
        assert_eq!(resolve(bare(Key::F12), &cx(focus)), run(View::SwitchDetail));
    }
}

fn focused_control(focus: FocusArea) -> KeyContext {
    KeyContext {
        value_control: true,
        ..cx(focus)
    }
}

#[test]
fn a_focused_control_takes_the_arrows_digits_and_entry_keys() {
    use super::super::commands::nav::Nav;
    for focus in [
        FocusArea::Session,
        FocusArea::Device,
        FocusArea::Browser,
        FocusArea::Arrangement,
    ] {
        let mut c = focused_control(focus);
        // A slot being selected must not take the arrows away from the control.
        c.slot_selected = true;
        assert_eq!(resolve(bare(Key::ArrowUp), &c), run(Nav::ValueUp));
        assert_eq!(resolve(bare(Key::ArrowDown), &c), run(Nav::ValueDown));
        assert_eq!(resolve(shift(Key::ArrowUp), &c), run(Nav::ValueUpFine));
        assert_eq!(resolve(shift(Key::ArrowDown), &c), run(Nav::ValueDownFine));
        assert_eq!(resolve(bare(Key::Delete), &c), run(Nav::ValueDefault));
        assert_eq!(resolve(bare(Key::Enter), &c), run(Nav::ValueConfirm));
        assert_eq!(resolve(bare(Key::Escape), &c), run(Nav::ValueCancel));
        assert_eq!(resolve(bare(Key::Num7), &c), run(Nav::Digit7));
        assert_eq!(resolve(bare(Key::Period), &c), run(Nav::NextField));
        assert_eq!(resolve(bare(Key::Comma), &c), run(Nav::NextField));
        assert_eq!(resolve(bare(Key::Minus), &c), run(Nav::Minus));
    }
}

#[test]
fn value_keys_do_nothing_extra_without_a_focused_control() {
    use super::super::commands::track::Track;
    let c = cx(FocusArea::Session);
    assert_eq!(resolve(bare(Key::Num0), &c), run(Track::ToggleActivator));
    assert_eq!(resolve(bare(Key::Num7), &c), Resolution::Unbound);
}

#[test]
fn digits_for_values_work_on_azerty_where_the_row_types_symbols() {
    use super::super::commands::nav::Nav;
    // The key labelled 7 on AZERTY types "è": the keymap goes by the physical key.
    let chord = chord_of(Key::Minus, Some(Key::Num6), Modifiers::NONE, MAC);
    assert_eq!(chord.key, Key::Num6);
    let c = focused_control(FocusArea::Device);
    assert_eq!(resolve(chord, &c), run(Nav::Digit6));
}

#[test]
fn the_grid_arrows_need_a_slot_scene_or_title() {
    use super::super::commands::launch::Launch;
    let plain = cx(FocusArea::Session);
    assert_eq!(resolve(bare(Key::ArrowUp), &plain), Resolution::Unbound);
    let slot = KeyContext {
        slot_selected: true,
        ..plain
    };
    assert_eq!(resolve(bare(Key::ArrowUp), &slot), run(Launch::ArrowUp));
    assert_eq!(
        resolve(bare(Key::ArrowRight), &slot),
        run(Launch::ArrowRight)
    );
    let scene = KeyContext {
        scene_selected: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::ArrowDown), &scene),
        run(Launch::ArrowDown)
    );
    assert_eq!(
        resolve(bare(Key::ArrowLeft), &scene),
        run(Launch::ArrowLeft)
    );
    let title = KeyContext {
        track_title: true,
        ..plain
    };
    assert_eq!(
        resolve(bare(Key::ArrowRight), &title),
        run(Launch::ArrowRight)
    );
    assert_eq!(
        resolve(bare(Key::ArrowDown), &title),
        run(Launch::ArrowDown)
    );
    assert_eq!(resolve(bare(Key::PageDown), &plain), run(Launch::PageDown));
    assert_eq!(resolve(bare(Key::Home), &slot), run(Launch::SlotHome));
    assert_eq!(resolve(bare(Key::End), &title), run(Launch::SlotEnd));
}

#[test]
fn home_and_end_stay_the_transport_keys_outside_the_grid() {
    use super::super::commands::transport::Transport;
    let c = cx(FocusArea::Session);
    assert_eq!(
        resolve(bare(Key::Home), &c),
        run(Transport::InsertMarkerToStart)
    );
}

#[test]
fn tab_moves_focus_only_in_tab_mode() {
    use super::super::commands::nav::Nav;
    use super::super::commands::view::View;
    let off = cx(FocusArea::Session);
    assert_eq!(resolve(bare(Key::Tab), &off), run(View::ToggleMainView));
    assert_eq!(resolve(shift(Key::Tab), &off), run(View::SwitchDetail));
    let on = KeyContext {
        tab_focus: true,
        ..off
    };
    assert_eq!(resolve(bare(Key::Tab), &on), run(Nav::NextControl));
    assert_eq!(resolve(shift(Key::Tab), &on), run(Nav::PrevControl));
}

#[test]
fn neighbor_keys_are_for_the_mixers_and_leave_clip_tabs_alone() {
    use super::super::commands::clip::Clip;
    use super::super::commands::nav::Nav;
    let alt_tab = chord_of(Key::Tab, Some(Key::Tab), Modifiers::ALT, MAC);
    let mut c = cx(FocusArea::Session);
    c.platform = MAC;
    c.tab_focus = true;
    assert_eq!(resolve(alt_tab, &c), run(Nav::NextNeighbor));
    let mut clip = cx(FocusArea::Clip);
    clip.platform = MAC;
    clip.tab_focus = true;
    assert_eq!(
        resolve(alt_tab, &clip),
        run(Clip::CycleEditorMode),
        "the Clip View's tab shortcut is still there in Tab mode"
    );
}

#[test]
fn tab_goes_through_names_while_renaming() {
    use super::super::commands::nav::Nav;
    let mut c = cx(FocusArea::Session);
    c.renaming = true;
    c.keyboard_taken = true;
    c.text_focused = true;
    assert_eq!(resolve(bare(Key::Tab), &c), run(Nav::RenameNext));
    // Other bare keys stay with the text field.
    assert_eq!(resolve(bare(Key::Delete), &c), Resolution::Unbound);
}

#[test]
fn tab_is_the_dialogs_own_while_one_is_open() {
    let mut c = cx(FocusArea::Session);
    c.dialog_open = true;
    assert_eq!(resolve(bare(Key::Tab), &c), Resolution::Unbound);
}

#[test]
fn delete_on_a_title_bar_deletes_the_track_but_not_otherwise() {
    use super::super::commands::edit::Edit;
    let plain = cx(FocusArea::Session);
    assert_eq!(resolve(bare(Key::Delete), &plain), run(Edit::Delete));
    let mut title = plain;
    title.track_title = true;
    assert_eq!(
        resolve(bare(Key::Delete), &title),
        run(Track::DeleteFromTitle)
    );
}

#[test]
fn cmd_arrows_move_nonadjacent_tracks_from_the_title_bar() {
    let plain = cx(FocusArea::Session);
    assert_eq!(resolve(cmd(Key::ArrowLeft), &plain), run(Track::MoveLeft));
    let mut title = plain;
    title.track_title = true;
    assert_eq!(
        resolve(cmd(Key::ArrowLeft), &title),
        run(Track::MoveTracksLeft)
    );
    assert_eq!(
        resolve(cmd(Key::ArrowRight), &title),
        run(Track::MoveTracksRight)
    );
    let mut arr = cx(FocusArea::Arrangement);
    arr.track_title = true;
    assert_eq!(resolve(cmd(Key::ArrowUp), &arr), run(Track::MoveTracksUp));
    assert_eq!(
        resolve(cmd(Key::ArrowDown), &arr),
        run(Track::MoveTracksDown)
    );
}

#[test]
fn escape_reaches_the_title_bar_from_inside_a_track() {
    use super::super::commands::nav::Nav;
    let mut c = cx(FocusArea::Session);
    c.in_track = true;
    assert_eq!(resolve(bare(Key::Escape), &c), run(Nav::JumpToTitle));
    let mut arr = cx(FocusArea::Arrangement);
    arr.in_track = true;
    assert_eq!(resolve(bare(Key::Escape), &arr), run(Nav::JumpToTitle));
}

#[test]
fn escape_still_closes_a_dialog_first() {
    use super::super::commands::view::View;
    let mut c = cx(FocusArea::Session);
    c.in_track = true;
    c.dialog_open = true;
    assert_eq!(resolve(bare(Key::Escape), &c), run(View::CloseDialog));
}

#[test]
fn left_and_right_step_devices_in_the_device_view() {
    use super::super::commands::nav::Nav;
    let c = cx(FocusArea::Device);
    assert_eq!(resolve(bare(Key::ArrowLeft), &c), run(Nav::DeviceLeft));
    assert_eq!(resolve(bare(Key::ArrowRight), &c), run(Nav::DeviceRight));
}
