//! The index as the UI uses it: through the channel, against generated folders.

use rlive_browser::catalog::{self, PluginEntry, MAX_FOR_LIVE_DEVICES};
use rlive_browser::item::{Item, ItemKey, ItemKind, Label, LabelSet};
use rlive_browser::library::COLLECTION_COUNT;
use rlive_browser::query::{complete_hashtag, trailing_hashtag};
use rlive_browser::tags::{factory_group_named, factory_tag};
use rlive_browser::{BrowserDb, Config, Op, Query, Scope, TagId};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rlive-browser-{name}-{}-{:?}",
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

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"").unwrap();
}

fn config(root: &Path) -> Config {
    Config {
        data_dir: root.join("data"),
        user_library: root.join("library"),
        audio_extensions: vec!["wav".into(), "aif".into(), "mp3".into()],
    }
}

fn wait(db: &mut BrowserDb, what: &str, done: impl Fn(&BrowserDb) -> bool) {
    let start = Instant::now();
    loop {
        db.poll();
        if done(db) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle(db: &mut BrowserDb) {
    db.flush_blocking();
    wait(db, "scans", |d| !d.scanning());
    db.flush_blocking();
}

fn names(db: &mut BrowserDb, query: Query) -> Vec<String> {
    let id = db.query(query);
    wait(db, "results", |d| d.results().id >= id);
    db.results()
        .rows
        .iter()
        .map(|r| r.item.name.clone())
        .collect()
}

fn search(db: &mut BrowserDb, text: &str) -> Vec<String> {
    names(
        db,
        Query {
            text: text.to_string(),
            ..Query::default()
        },
    )
}

fn sample(key: &str, name: &str) -> Item {
    let mut item = Item::new(ItemKey(key.to_string()), name, ItemKind::Sample);
    item.labels = LabelSet::of(&[Label::Samples]);
    item
}

fn open(temp: &Temp) -> BrowserDb {
    let mut db = BrowserDb::open(config(&temp.0));
    settle(&mut db);
    db
}

#[test]
fn search_needs_every_word_and_ignores_case() {
    let temp = Temp::new("and");
    let mut db = open(&temp);
    db.set_catalog(
        "t",
        vec![
            sample("a", "Electric Bass Round"),
            sample("b", "Electric Piano"),
            sample("c", "Upright Bass"),
        ],
    );
    assert_eq!(search(&mut db, "electric bass"), ["Electric Bass Round"]);
    assert_eq!(
        search(&mut db, "BASS"),
        ["Electric Bass Round", "Upright Bass"]
    );
    assert!(search(&mut db, "electric bass piano").is_empty());
}

#[test]
fn a_hashtag_finds_what_is_tagged_and_its_subtags() {
    let temp = Temp::new("hash");
    let mut db = open(&temp);
    let sounds = factory_group_named("Sounds").unwrap();
    let bass = factory_tag("Bass");
    db.set_catalog(
        "t",
        vec![sample("a", "One"), sample("b", "Two"), sample("c", "Three")],
    );
    db.op(Op::Assign {
        items: vec![ItemKey("a".into())],
        tag: bass,
        on: true,
    });
    db.op(Op::AddTag {
        group: sounds,
        parent: Some(bass),
        name: "Acid Bass".into(),
        assign: vec![ItemKey("b".into())],
    });
    assert_eq!(search(&mut db, "#bass"), ["One", "Two"]);
    assert_eq!(search(&mut db, "#Acid_Bass"), ["Two"]);
    assert_eq!(search(&mut db, "#bass two"), ["Two"]);
    assert!(search(&mut db, "#no_such_tag").is_empty());
}

#[test]
fn hashtag_autocomplete_finishes_the_last_word() {
    assert_eq!(trailing_hashtag("kick #dr"), Some("dr"));
    assert_eq!(trailing_hashtag("kick #dr "), None);
    assert_eq!(trailing_hashtag("kick"), None);
    assert_eq!(
        complete_hashtag("kick #dr", "Drum Loop"),
        "kick #Drum_Loop "
    );
    let db = rlive_browser::tags::TagDb::default();
    let names: Vec<String> = db.completions("dr").into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["Drum Kit", "Drum Loop", "Drum Machine"]);
}

#[test]
fn filters_combine_or_within_a_group_and_across_groups_and() {
    let temp = Temp::new("filters");
    let mut db = open(&temp);
    db.set_catalog(
        "t",
        vec![sample("a", "A"), sample("b", "B"), sample("c", "C")],
    );
    let (bass, keys, warm) = (
        factory_tag("Bass"),
        factory_tag("Keys"),
        factory_tag("Warm"),
    );
    for (item, tag) in [("a", bass), ("b", keys), ("c", keys), ("c", warm)] {
        db.op(Op::Assign {
            items: vec![ItemKey(item.into())],
            tag,
            on: true,
        });
    }
    let filtered = |tags: &[TagId]| Query {
        tags: tags.iter().copied().collect(),
        ..Query::default()
    };
    assert_eq!(names(&mut db, filtered(&[bass, keys])), ["A", "B", "C"]);
    assert_eq!(names(&mut db, filtered(&[bass, keys, warm])), ["C"]);
}

#[test]
fn tags_add_to_what_an_item_comes_with_and_factory_tags_stay() {
    let temp = Temp::new("factory");
    let mut db = open(&temp);
    let mut item = sample("a", "Factory Thing");
    let creator = factory_tag("rLive");
    item.factory_tags = vec![creator];
    db.set_catalog("t", vec![item]);
    let warm = factory_tag("Warm");
    db.op(Op::Assign {
        items: vec![ItemKey("a".into())],
        tag: warm,
        on: true,
    });
    db.op(Op::Assign {
        items: vec![ItemKey("a".into())],
        tag: creator,
        on: false,
    });
    db.flush_blocking();
    let quick = db
        .library()
        .tags
        .quick_tags(&[(ItemKey("a".into()), vec![creator])]);
    let summary: Vec<(&str, bool)> = quick
        .iter()
        .map(|q| (q.name.as_str(), q.removable))
        .collect();
    assert_eq!(summary, [("rLive", false), ("Warm", true)]);
    assert_eq!(search(&mut db, "#rLive"), ["Factory Thing"]);
}

#[test]
fn quick_tags_mark_what_only_part_of_the_selection_has() {
    let temp = Temp::new("quick");
    let mut db = open(&temp);
    let (warm, soft) = (factory_tag("Warm"), factory_tag("Soft"));
    let (a, b) = (ItemKey("a".into()), ItemKey("b".into()));
    db.op(Op::Assign {
        items: vec![a.clone(), b.clone()],
        tag: warm,
        on: true,
    });
    db.op(Op::Assign {
        items: vec![a.clone()],
        tag: soft,
        on: true,
    });
    db.flush_blocking();
    let quick = db.library().tags.quick_tags(&[(a, vec![]), (b, vec![])]);
    let summary: Vec<(&str, bool)> = quick.iter().map(|q| (q.name.as_str(), q.partial)).collect();
    assert_eq!(summary, [("Soft", true), ("Warm", false)]);
}

#[test]
fn custom_groups_and_tags_can_be_edited_and_factory_ones_cannot() {
    let mut db = rlive_browser::tags::TagDb::default();
    let group = db.add_group("Field Recording").unwrap();
    assert!(db.add_group("Field recording").is_err(), "names are unique");
    let rain = db.add_tag(group, None, "Rain").unwrap();
    let drizzle = db.add_tag(group, Some(rain), "Drizzle").unwrap();
    assert!(db.descendants(rain).contains(&drizzle));
    db.rename_tag(rain, "Weather").unwrap();
    assert_eq!(db.tag(rain).unwrap().name, "Weather");
    let key = ItemKey("x".into());
    db.assign(std::slice::from_ref(&key), drizzle, true);
    db.delete_tag(rain).unwrap();
    assert!(db.tag(drizzle).is_none(), "subtags go with their parent");
    assert!(db.assigned.is_empty(), "and so do their assignments");

    let bass = factory_tag("Bass");
    assert!(db.rename_tag(bass, "Low").is_err());
    assert!(db.delete_tag(bass).is_err());
    assert!(db
        .delete_group(factory_group_named("Sounds").unwrap())
        .is_err());
    let creator = factory_group_named("Creator").unwrap();
    assert!(
        db.add_tag(creator, Some(factory_tag("rLive")), "Me")
            .is_err(),
        "no subtags in Creator"
    );
    db.delete_group(group).unwrap();
    assert!(db.group(group).is_none());
}

#[test]
fn collections_hold_several_colours_and_zero_clears_them() {
    let temp = Temp::new("colors");
    let mut db = open(&temp);
    db.set_catalog("t", vec![sample("a", "A"), sample("b", "B")]);
    let items = vec![ItemKey("a".into())];
    for collection in [0u8, 2, 5] {
        db.op(Op::SetColor {
            items: items.clone(),
            collection,
            on: true,
        });
    }
    db.flush_blocking();
    assert_eq!(db.library().colors_of(&items[0]), 0b100101);
    assert!(
        db.library().collections[5].visible,
        "a hidden colour shows up once it's used"
    );
    assert_eq!(
        names(
            &mut db,
            Query {
                scope: Scope::Collection(2),
                ..Query::default()
            }
        ),
        ["A"]
    );
    db.op(Op::ClearColors(items.clone()));
    db.flush_blocking();
    assert_eq!(db.library().colors_of(&items[0]), 0);
    assert!(db.library().collections[5].visible, "and stays shown");
    assert_eq!(db.library().collections.len(), COLLECTION_COUNT);
}

#[test]
fn everything_comes_back_after_a_restart() {
    let temp = Temp::new("persist");
    let warm = factory_tag("Warm");
    let key = ItemKey("a".into());
    {
        let mut db = open(&temp);
        db.set_catalog("t", vec![sample("a", "A")]);
        db.op(Op::Assign {
            items: vec![key.clone()],
            tag: warm,
            on: true,
        });
        db.op(Op::SetColor {
            items: vec![key.clone()],
            collection: 3,
            on: true,
        });
        db.op(Op::AddCustomLabel {
            name: "Warm things".into(),
            scope: Scope::Label(Label::All),
            text: String::new(),
            tags: BTreeSet::from([warm]),
        });
        db.op(Op::SetFilterSelection(
            "label:Sounds".into(),
            BTreeSet::from([warm]),
        ));
        db.op(Op::RenameLabel("Sounds".into(), "Timbres".into()));
        db.op(Op::AddFolder(temp.0.join("nowhere")));
        db.op(Op::SetShowExtensions(true));
        settle(&mut db);
    }
    let mut db = open(&temp);
    db.set_catalog("t", vec![sample("a", "A")]);
    let lib = db.library().clone();
    assert!(lib.tags.assigned[&key].contains(&warm));
    assert_eq!(lib.colors_of(&key), 1 << 3);
    assert_eq!(lib.custom_labels[0].name, "Warm things");
    assert_eq!(lib.filters["label:Sounds"].selected, BTreeSet::from([warm]));
    assert_eq!(lib.label_name("Sounds", "Sounds"), "Timbres");
    assert!(lib.content.show_extensions);
    assert_eq!(lib.folders[0].path, temp.0.join("nowhere"));
    let id = lib.custom_labels[0].id;
    assert_eq!(
        names(
            &mut db,
            Query {
                scope: Scope::Custom(id),
                ..Query::default()
            }
        ),
        ["A"]
    );
}

#[test]
fn a_user_folder_is_indexed_removed_and_located_again() {
    let temp = Temp::new("folders");
    let folder = temp.0.join("Field Recordings");
    touch(&folder.join("rain").join("drizzle.wav"));
    touch(&folder.join("thunder.mp3"));
    touch(&folder.join("notes.txt"));
    let mut db = open(&temp);
    db.op(Op::AddFolder(folder.clone()));
    settle(&mut db);
    let place = Query {
        scope: Scope::Place(folder.clone()),
        ..Query::default()
    };
    assert_eq!(names(&mut db, place.clone()), ["rain", "thunder"]);
    assert_eq!(search(&mut db, "drizzle"), ["drizzle"]);
    assert!(!db.scan_of(&folder).unwrap().missing);

    let key = ItemKey::file(&folder.join("thunder.mp3"));
    db.op(Op::SetColor {
        items: vec![key.clone()],
        collection: 1,
        on: true,
    });
    db.flush_blocking();

    let moved = temp.0.join("Moved");
    std::fs::rename(&folder, &moved).unwrap();
    db.rescan();
    settle(&mut db);
    assert!(db.scan_of(&folder).unwrap().missing, "greyed out in Places");

    db.op(Op::LocateFolder(folder.clone(), moved.clone()));
    settle(&mut db);
    assert_eq!(db.library().folders[0].path, moved);
    let new_key = ItemKey::file(&moved.join("thunder.mp3"));
    assert_eq!(db.library().colors_of(&new_key), 1 << 1, "colours followed");
    assert_eq!(search(&mut db, "drizzle"), ["drizzle"]);

    db.op(Op::RemoveFolder(moved.clone()));
    settle(&mut db);
    assert!(db.library().folders.is_empty());
    assert!(search(&mut db, "drizzle").is_empty(), "forgotten");
    assert!(moved.join("thunder.mp3").exists(), "and left on disk");
}

#[test]
fn saved_files_drop_out_when_deleted() {
    let temp = Temp::new("stale");
    let folder = temp.0.join("F");
    touch(&folder.join("a.wav"));
    touch(&folder.join("b.wav"));
    let mut db = open(&temp);
    db.op(Op::AddFolder(folder.clone()));
    settle(&mut db);
    assert_eq!(search(&mut db, "wav").len(), 2);
    std::fs::remove_file(folder.join("a.wav")).unwrap();
    db.rescan();
    settle(&mut db);
    assert_eq!(search(&mut db, "wav"), ["b"]);
}

#[test]
fn the_library_labels_list_what_the_manual_says() {
    let temp = Temp::new("labels");
    let mut db = open(&temp);
    db.set_catalog("builtin", catalog::builtin_items());
    let in_label = |db: &mut BrowserDb, label| {
        names(
            db,
            Query {
                scope: Scope::Label(label),
                ..Query::default()
            },
        )
    };
    let instruments = in_label(&mut db, Label::Instruments);
    assert!(instruments.iter().any(|n| n == "Simpler"));
    assert!(!instruments.iter().any(|n| n == "Reverb"));
    let effects = in_label(&mut db, Label::AudioEffects);
    assert!(effects.iter().any(|n| n == "Reverb"));
    let midi = in_label(&mut db, Label::MidiEffects);
    assert!(midi.iter().any(|n| n == "Chord"));
    let max = in_label(&mut db, Label::MaxForLive);
    assert!(!max.is_empty());
    assert!(max.len() <= MAX_FOR_LIVE_DEVICES.len());
    for id in MAX_FOR_LIVE_DEVICES {
        assert!(rlive_devices::find(id).is_some(), "{id} is in the registry");
    }
}

#[test]
fn device_folders_open_to_show_their_presets() {
    let temp = Temp::new("tree");
    let mut db = open(&temp);
    db.set_catalog("builtin", catalog::builtin_items());
    let scope = Scope::Label(Label::AudioEffects);
    let id = db.query(Query {
        scope: scope.clone(),
        ..Query::default()
    });
    wait(&mut db, "results", |d| d.results().id >= id);
    let reverb = db
        .results()
        .rows
        .iter()
        .find(|r| r.item.name == "Reverb")
        .unwrap()
        .clone();
    assert!(reverb.has_children);
    assert!(!reverb.expanded);
    let open = names(
        &mut db,
        Query {
            scope,
            expanded: BTreeSet::from([reverb.item.key.clone()]),
            ..Query::default()
        },
    );
    let at = open.iter().position(|n| n == "Reverb").unwrap();
    assert_eq!(open[at + 1], "Cathedral of Dust");
}

#[test]
fn vst3_subcategories_become_tags_where_they_map() {
    let plugin = |subcategories: &str| PluginEntry {
        format: "VST3".into(),
        id: "abc".into(),
        name: "P".into(),
        vendor: "V".into(),
        is_instrument: false,
        subcategories: subcategories.into(),
    };
    let tags = |s: &str| catalog::plugin_items(&[plugin(s)])[1].factory_tags.clone();
    assert_eq!(tags("Fx|Dynamics"), [factory_tag("Dynamics")]);
    assert_eq!(tags("Fx|Delay|Reverb"), {
        let mut v = vec![
            factory_tag("Delay & Modulation"),
            factory_tag("Reverb & Resonance"),
        ];
        v.sort();
        v
    });
    assert_eq!(tags("Instrument|Synth"), [factory_tag("Synthesizer")]);
    assert!(tags("Fx|Weird").is_empty(), "no mapping, no tag");
}

/// The acceptance test for keeping large folders off the UI thread: however big the tree, every
/// call the UI makes returns at once, results arrive while the scan runs, and the scan finishes.
#[test]
fn indexing_a_large_tree_never_blocks_the_caller() {
    let temp = Temp::new("large");
    let folder = temp.0.join("Huge");
    let mut count = 0;
    for d in 0..20 {
        for s in 0..5 {
            let dir = folder.join(format!("d{d}")).join(format!("s{s}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..100 {
                std::fs::write(dir.join(format!("loop {d} {s} {f}.wav")), b"").unwrap();
                count += 1;
            }
        }
    }
    let mut db = open(&temp);

    let limit = Duration::from_millis(100);
    let timed = |what: &str, f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        assert!(t.elapsed() < limit, "{what} took {:?}", t.elapsed());
    };
    timed("adding the folder", &mut || {
        db.op(Op::AddFolder(folder.clone()));
    });

    let mut saw_progress = false;
    let start = Instant::now();
    loop {
        timed("polling", &mut || {
            db.poll();
        });
        timed("querying", &mut || {
            db.query(Query {
                text: "loop".into(),
                ..Query::default()
            });
        });
        timed("tagging", &mut || {
            db.op(Op::SetShowExtensions(false));
        });
        let progress = db
            .scan_of(&folder)
            .is_some_and(|s| s.running && s.files > 0);
        saw_progress |= progress;
        if !db.scanning() && db.scan_of(&folder).is_some_and(|s| !s.running) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "scan hung");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(saw_progress, "the scan reported its progress while it ran");
    settle(&mut db);
    assert_eq!(search(&mut db, "loop").len(), count);
    assert!(
        db.scan_of(&folder).unwrap().files > count,
        "folders count too"
    );
}

#[test]
fn custom_labels_can_be_renamed_iconed_and_deleted() {
    let temp = Temp::new("custom");
    let mut db = open(&temp);
    db.set_catalog("t", vec![sample("a", "Warm Pad")]);
    db.op(Op::AddCustomLabel {
        name: "Pads".into(),
        scope: Scope::Label(Label::All),
        text: "pad".into(),
        tags: BTreeSet::new(),
    });
    db.flush_blocking();
    let id = db.library().custom_labels[0].id;
    db.op(Op::RenameCustomLabel(id, "My Pads".into()));
    db.op(Op::SetCustomLabelIcon(id, "Star".into()));
    db.flush_blocking();
    assert_eq!(db.library().custom_labels[0].name, "My Pads");
    assert_eq!(db.library().custom_labels[0].icon, "Star");
    db.op(Op::DeleteCustomLabel(id));
    db.flush_blocking();
    assert!(db.library().custom_labels.is_empty());
}

#[test]
fn filter_groups_hide_per_label_and_reset_to_their_defaults() {
    let temp = Temp::new("groups");
    let mut db = open(&temp);
    let scope = Scope::Label(Label::Sounds);
    let sounds = factory_group_named("Sounds").unwrap();
    let character = factory_group_named("Character").unwrap();
    db.flush_blocking();
    let defaults = db.library().visible_groups(&scope);
    assert!(defaults.contains(&sounds) && defaults.contains(&character));
    db.op(Op::SetFilterGroups(
        scope.storage_key(),
        Some(BTreeSet::from([sounds])),
    ));
    db.flush_blocking();
    assert_eq!(db.library().visible_groups(&scope), [sounds]);
    assert!(
        db.library()
            .visible_groups(&Scope::Label(Label::Samples))
            .contains(&character),
        "other labels keep their own"
    );
    db.op(Op::SetFilterGroups(scope.storage_key(), None));
    db.flush_blocking();
    assert_eq!(db.library().visible_groups(&scope), defaults);
}

#[test]
fn a_custom_group_shows_in_the_filter_view_once_it_is_used() {
    let temp = Temp::new("used");
    let mut db = open(&temp);
    db.op(Op::AddGroup("Field".into()));
    db.flush_blocking();
    let group = *db
        .library()
        .tags
        .all_groups()
        .iter()
        .find(|g| g.name == "Field")
        .map(|g| &g.id)
        .unwrap();
    let scope = Scope::Label(Label::Samples);
    assert!(!db.library().visible_groups(&scope).contains(&group));
    db.op(Op::AddTag {
        group,
        parent: None,
        name: "Rain".into(),
        assign: vec![ItemKey("x".into())],
    });
    db.flush_blocking();
    assert!(db.library().visible_groups(&scope).contains(&group));
}

#[test]
fn tunings_and_grooves_are_listed_with_the_users_under_the_user_tag() {
    let temp = Temp::new("assets");
    let folder = temp.0.join("Mine");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("My Scale.scl"),
        "! my.scl\n!\nA user scale\n 2\n!\n 386.0\n 2/1\n",
    )
    .unwrap();
    touch(&folder.join("Swing.rgroove"));
    let mut db = open(&temp);
    db.set_catalog("builtin", catalog::builtin_items());
    db.op(Op::AddFolder(folder.clone()));
    settle(&mut db);
    let label = |label| Query {
        scope: Scope::Label(label),
        ..Query::default()
    };
    let tunings = names(&mut db, label(Label::Tunings));
    assert!(tunings.iter().any(|n| n == "My Scale"));
    assert!(tunings.len() > 1, "with the factory set");
    let row = db
        .results()
        .rows
        .iter()
        .find(|r| r.item.name == "My Scale")
        .unwrap()
        .clone();
    assert_eq!(row.item.factory_tags, [factory_tag("User")]);
    assert!(
        row.item.vendor.contains("A user scale"),
        "{}",
        row.item.vendor
    );
    assert_eq!(
        names(
            &mut db,
            Query {
                scope: Scope::Label(Label::Tunings),
                tags: BTreeSet::from([factory_tag("User")]),
                ..Query::default()
            }
        ),
        ["My Scale"]
    );
    let grooves = names(&mut db, label(Label::Grooves));
    assert!(grooves.iter().any(|n| n == "Swing"));
    assert!(grooves.len() > 1);
}

#[test]
fn chord_banks_are_found_by_the_stacks_tag() {
    let temp = Temp::new("stacks");
    let folder = temp.0.join("Banks");
    touch(&folder.join("Mine.stacks"));
    let mut db = open(&temp);
    db.op(Op::AddFolder(folder.clone()));
    settle(&mut db);
    assert_eq!(
        names(
            &mut db,
            Query {
                tags: BTreeSet::from([factory_tag("Stacks")]),
                ..Query::default()
            }
        ),
        ["Mine"]
    );
}

#[test]
fn a_late_answer_never_replaces_a_newer_one() {
    let temp = Temp::new("late");
    let mut db = open(&temp);
    db.set_catalog("t", vec![sample("a", "Alpha"), sample("b", "Beta")]);
    let first = db.query(Query {
        text: "alpha".into(),
        ..Query::default()
    });
    let second = db.query(Query {
        text: "beta".into(),
        ..Query::default()
    });
    assert!(second > first);
    wait(&mut db, "answer", |d| d.results().id >= second);
    assert_eq!(db.results().rows[0].item.name, "Beta");
}
