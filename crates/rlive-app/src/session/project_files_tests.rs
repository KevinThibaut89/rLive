//! Project folders, the File Manager's work, Export's options, MIDI files and Slice to MIDI.

use super::*;
use rlive_audio_io::{decode, probe, BitDepth, Dither, ExportType, PcmFormat};
use rlive_core::PPQ;
use rlive_engine::{create_engine, Engine};
use rlive_project::project_folder::{self, MARKER};
use rlive_project::{ArrangementClip, Clip, ClipContent, FileRef, NoteData, RackKind};
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: f64 = 48_000.0;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rlive-c13-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }

    fn file(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn write_tone(path: &Path, seconds: f64, level: f32) {
    let n = (seconds * SR) as usize;
    let tone: Vec<f32> = (0..n)
        .map(|i| level * (i as f32 / SR as f32 * 440.0 * std::f32::consts::TAU).sin())
        .collect();
    rlive_devices::write_wav(path, &tone, &tone, SR as u32).unwrap();
}

/// Short bursts of noise at these seconds, in `length` seconds of silence.
fn write_hits(path: &Path, length: f64, hits: &[f64]) {
    let n = (length * SR) as usize;
    let mut left = vec![0.0f32; n];
    let mut seed = 22_222u32;
    for at in hits {
        let start = (at * SR) as usize;
        for i in 0..2_400.min(n - start) {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 16) as f32 / 32_768.0 - 1.0;
            left[start + i] = noise * 0.8 * (1.0 - i as f32 / 2_400.0);
        }
    }
    rlive_devices::write_wav(path, &left, &left, SR as u32).unwrap();
}

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, 512);
    let s = Session::new(handle, Project::empty(), PluginCache::default());
    (engine, s)
}

fn lib(dir: &Dir) -> rlive_browser::defaults::UserLibrary {
    let l = rlive_browser::defaults::UserLibrary::new(dir.file("Library"));
    l.ensure_folders().unwrap();
    l
}

fn audio_clip(path: &Path, beats: u64) -> Clip {
    Clip {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        length: beats * PPQ,
        content: ClipContent::audio(path.to_path_buf(), 0.0),
        ..Clip::default()
    }
}

fn audio_track(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Audio, None)
}

fn arrive(s: &mut Session) {
    let paths = s.project_audio_paths();
    s.samples.wait_for(paths.iter().map(PathBuf::as_path));
    s.poll_samples();
}

fn place(s: &mut Session, t: TrackId, path: &Path, beats: u64, at: u64) {
    s.arr_place(t, ArrangementClip::new(audio_clip(path, beats), at));
}

fn refs(s: &Session) -> Vec<PathBuf> {
    let mut out = Vec::new();
    s.project
        .for_each_file_ref(&mut |f: &FileRef| out.push(f.path.clone()));
    out
}

// ------------------------------------------------------------- the Project folder

#[test]
fn saving_a_new_set_makes_a_project_folder_and_versions_share_it() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let set = s.save_as(&dir.file("Tango.rlive")).unwrap();
    assert_eq!(set, dir.0.join("Tango Project/Tango.rlive"));
    assert!(dir.0.join("Tango Project").join(MARKER).exists());
    assert_eq!(s.project_name().as_deref(), Some("Tango Project"));
    assert_eq!(s.project_root(), Some(dir.0.join("Tango Project")));
    let piano = dir.0.join("Tango Project/Piano.rlive");
    assert_eq!(s.save_as(&piano).unwrap(), piano);
    assert_eq!(
        project_folder::find_root(&piano),
        Some(dir.0.join("Tango Project"))
    );
    assert!(
        dir.0.join("Tango Project/Tango.rlive").exists(),
        "both stay"
    );
}

#[test]
fn save_as_outside_a_project_starts_a_new_one_and_leaves_references_alone() {
    let dir = Dir::new();
    let tone = dir.file("Tango Project/Samples/Recorded/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    project_folder::mark(&dir.0.join("Tango Project")).unwrap();
    s.save(&dir.0.join("Tango Project/Tango.rlive")).unwrap();
    let electro = s.save_as(&dir.file("Electro.rlive")).unwrap();
    assert_eq!(electro, dir.0.join("Electro Project/Electro.rlive"));
    assert_eq!(
        refs(&s),
        std::slice::from_ref(&tone),
        "still pointing at the old Project"
    );
    assert!(tone.exists());
    assert!(!dir.0.join("Electro Project/Samples").exists());
}

#[test]
fn save_copy_writes_a_second_set_and_keeps_working_on_the_first() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let first = s.save_as(&dir.file("One.rlive")).unwrap();
    s.add_track(TrackKind::Audio, None);
    assert!(s.dirty);
    let copy = s.save_copy(&dir.file("Two.rlive")).unwrap();
    assert_eq!(copy, dir.0.join("Two Project/Two.rlive"));
    assert_eq!(s.path.as_deref(), Some(first.as_path()), "the same Set");
    assert!(s.dirty, "the first hasn't been saved");
    assert_eq!(
        Project::load(&copy).unwrap().tracks.len(),
        s.project.tracks.len()
    );
}

#[test]
fn a_set_from_an_older_format_has_to_be_saved_under_a_new_name() {
    let dir = Dir::new();
    let old = dir.file("Old.rlive");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rlive-project/tests/fixtures/v1/default.rlive"
        ),
        &old,
    )
    .unwrap();
    let (_e, mut s) = session();
    s.load(&old).unwrap();
    assert!(s.needs_save_as());
    let new = s.save_as(&dir.file("New.rlive")).unwrap();
    assert!(!s.needs_save_as());
    assert!(new.exists());
}

#[test]
fn files_moved_with_their_project_are_found_where_they_are_relative_to_it() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    let root = set.parent().unwrap().to_path_buf();
    let tone = root.join("Samples/Recorded/take.wav");
    std::fs::create_dir_all(tone.parent().unwrap()).unwrap();
    write_tone(&tone, 1.0, 0.4);
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    s.save(&set).unwrap();
    let moved = dir.0.join("Moved");
    std::fs::rename(&root, &moved).unwrap();
    let (_e2, mut other) = session();
    other.load(&moved.join("Song.rlive")).unwrap();
    assert_eq!(refs(&other), [moved.join("Samples/Recorded/take.wav")]);
    assert!(other.missing_files().is_empty());
}

// ------------------------------------------------------------- templates

#[test]
fn a_template_opens_as_a_new_untitled_set() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    s.library = Some(lib(&dir));
    let tracks = s.project.tracks.len();
    s.add_track(TrackKind::Audio, None);
    let template = s.save_as_template("Band").unwrap();
    assert_eq!(template, lib(&dir).templates_dir().join("Band.rlive"));
    assert_eq!(
        s.save_as_template("Band").unwrap(),
        lib(&dir).templates_dir().join("Band 2.rlive"),
        "a template of the same name isn't replaced"
    );
    s.new_project();
    s.load_as_untitled(&template).unwrap();
    assert_eq!(s.path, None, "a new Set, with no name");
    assert!(!s.dirty);
    assert_eq!(s.project.tracks.len(), tracks + 1);
}

#[test]
fn the_default_set_is_what_new_sets_start_as() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    s.library = Some(lib(&dir));
    s.new_from_default();
    let plain = s.project.tracks.len();
    s.add_track(TrackKind::Audio, None);
    let default = s.save_as_default_set().unwrap();
    assert!(default.exists());
    s.new_project();
    assert_eq!(s.project.tracks.len(), 3, "new_project alone is untouched");
    s.new_from_default();
    assert_eq!(s.project.tracks.len(), plain + 1);
    assert_eq!(s.path, None);
}

#[test]
fn any_template_can_be_made_the_default_and_the_old_one_is_kept() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    s.library = Some(lib(&dir));
    s.save_as_default_set().unwrap();
    s.add_track(TrackKind::Audio, None);
    let template = s.save_as_template("T").unwrap();
    s.set_default_set_from(&template).unwrap();
    assert!(
        rlive_browser::backup::folder_of(&lib(&dir).default_set_path())
            .unwrap()
            .read_dir()
            .unwrap()
            .count()
            >= 1,
        "the previous default is in Backup"
    );
    let n = s.project.tracks.len();
    s.new_project();
    s.new_from_default();
    assert_eq!(s.project.tracks.len(), n);
}

// ------------------------------------------------------------- missing files

#[test]
fn a_missing_file_is_reported_when_the_set_loads() {
    let dir = Dir::new();
    let tone = dir.file("elsewhere/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    std::fs::remove_file(&tone).unwrap();
    let (_e2, mut other) = session();
    other.load(&set).unwrap();
    assert_eq!(other.missing_files(), std::slice::from_ref(&tone));
    assert!(other.is_offline(&tone));
    let lane = &other.project.tracks[0].arrangement[0];
    assert!(other.clip_is_offline(ClipRef::Arr {
        track: other.project.tracks[0].id,
        id: lane.id
    }));
    let list = other.file_list();
    assert_eq!(list.len(), 1);
    assert!(list[0].missing);
}

#[test]
fn an_offline_clip_renders_as_silence_and_says_why() {
    let dir = Dir::new();
    let gone = dir.0.join("gone.wav");
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &gone, 4, 0);
    s.export_arrangement(&dir.file("x.wav")).unwrap();
    assert!(s.messages.iter().any(|m| m.contains("gone.wav")));
    let audio = rlive_devices::load_wav(&dir.file("x.wav")).unwrap();
    assert!(audio.left.iter().all(|x| *x == 0.0));
}

#[test]
fn searching_relinks_a_moved_file_with_one_candidate() {
    let dir = Dir::new();
    let tone = dir.file("Old/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    arrive(&mut s);
    let moved = dir.file("New/deep/a.wav");
    std::fs::rename(&tone, &moved).unwrap();
    s.missing.insert(tone.clone());
    let report = s.search_missing(&SearchOptions {
        folder: Some(dir.0.join("New")),
        ..SearchOptions::default()
    });
    assert_eq!(report.relinked, [(tone.clone(), moved.clone())]);
    assert_eq!(refs(&s), std::slice::from_ref(&moved));
    assert!(s.missing_files().is_empty());
    arrive(&mut s);
    assert!(s.samples.is_ready(&moved), "and it plays again");
}

#[test]
fn searching_leaves_several_candidates_to_the_user_and_none_stays_missing() {
    let dir = Dir::new();
    let (a, b) = (dir.file("Old/a.wav"), dir.file("Old/b.wav"));
    write_tone(&a, 1.0, 0.4);
    write_tone(&b, 1.0, 0.3);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &a, 4, 0);
    place(&mut s, t, &b, 4, 16 * PPQ);
    arrive(&mut s);
    s.missing.insert(a.clone());
    s.missing.insert(b.clone());
    std::fs::remove_file(&b).unwrap();
    let sz = std::fs::metadata(&a).unwrap().len() as usize;
    std::fs::write(dir.file("Found/one/a.wav"), vec![1u8; sz]).unwrap();
    std::fs::write(dir.file("Found/two/a.wav"), vec![2u8; sz]).unwrap();
    let report = s.search_missing(&SearchOptions {
        folder: Some(dir.0.join("Found")),
        ..SearchOptions::default()
    });
    assert_eq!(report.relinked, []);
    assert_eq!(report.not_found, std::slice::from_ref(&b));
    assert_eq!(report.choices[0].0, a);
    assert_eq!(s.candidates_for(&a).len(), 2);
    assert_eq!(s.missing_files().len(), 2, "still missing until chosen");
    let pick = s.candidates_for(&a)[1].clone();
    s.relink_file(&a, &pick);
    assert!(s.candidates_for(&a).is_empty());
}

#[test]
fn a_search_finds_a_file_in_a_project_or_the_library() {
    let dir = Dir::new();
    let tone = dir.file("Gone/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    s.library = Some(lib(&dir));
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    s.save_as(&dir.file("Song.rlive")).unwrap();
    arrive(&mut s);
    let in_library = dir.0.join("Library/Samples/a.wav");
    std::fs::rename(&tone, &in_library).unwrap();
    s.missing.insert(tone.clone());
    let report = s.search_missing(&SearchOptions {
        project: true,
        ..SearchOptions::default()
    });
    assert_eq!(
        report.not_found,
        std::slice::from_ref(&tone),
        "it isn't in the Project"
    );
    let report = s.search_missing(&SearchOptions {
        library: true,
        ..SearchOptions::default()
    });
    assert_eq!(report.relinked, [(tone.clone(), in_library)]);
}

#[test]
fn a_manual_repair_takes_any_file_without_asking_what_it_is() {
    let dir = Dir::new();
    let gone = dir.0.join("gone.wav");
    let other = dir.file("other.wav");
    write_tone(&other, 1.0, 0.1);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &gone, 4, 0);
    arrive(&mut s);
    s.refresh_missing();
    assert_eq!(s.relink_file(&gone, &other), 1);
    assert_eq!(refs(&s), [other]);
    assert!(s.missing_files().is_empty());
}

#[test]
fn replacing_a_file_drops_warp_markers_only_when_it_is_shorter() {
    let dir = Dir::new();
    let long = dir.file("long.wav");
    let same = dir.file("same.wav");
    let short = dir.file("short.wav");
    write_tone(&long, 2.0, 0.4);
    write_tone(&same, 2.0, 0.2);
    write_tone(&short, 1.0, 0.2);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    let mut clip = audio_clip(&long, 4);
    if let ClipContent::Audio { warp, .. } = &mut clip.content {
        warp.enabled = true;
        warp.markers = vec![
            rlive_project::WarpMarker {
                seconds: 0.0,
                tick: 0.0,
            },
            rlive_project::WarpMarker {
                seconds: 1.5,
                tick: 3_000.0,
            },
        ];
    }
    s.arr_place(t, ArrangementClip::new(clip, 0));
    arrive(&mut s);
    let markers = |s: &Session| match &s.project.tracks[0].arrangement[0].clip.content {
        ClipContent::Audio { warp, .. } => warp.markers.clone(),
        _ => panic!(),
    };
    let before = markers(&s);
    s.replace_file(&long, &same);
    arrive(&mut s);
    assert_eq!(markers(&s), before, "same length: markers stay");
    s.replace_file(&same, &short);
    assert_ne!(markers(&s), before, "shorter: they go");
    assert_eq!(refs(&s), [short]);
}

// ------------------------------------------------------------- Collect All

#[test]
fn collect_all_copies_the_files_and_moves_the_references_and_leaves_the_originals() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/kick.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    let root = set.parent().unwrap().to_path_buf();
    let summary = s.location_summaries();
    assert_eq!(summary[0].location, FileLocation::Elsewhere);
    assert_eq!(summary[0].files, 1);
    let before = std::fs::read(&tone).unwrap();
    let report = s.collect_all_and_save(CollectOptions::default()).unwrap();
    let copied = root.join("Samples/Imported/kick.wav");
    assert_eq!(report.copied, 1);
    assert!(report.bytes > 0);
    assert_eq!(std::fs::read(&copied).unwrap(), before);
    assert_eq!(
        std::fs::read(&tone).unwrap(),
        before,
        "the original is untouched"
    );
    assert_eq!(refs(&s), std::slice::from_ref(&copied));
    assert!(!s.dirty, "and it saved");
    let saved = Project::load(&set).unwrap();
    let mut saved_refs = Vec::new();
    saved.for_each_file_ref(&mut |f: &FileRef| {
        saved_refs.push((f.path.clone(), f.relative.clone()))
    });
    assert_eq!(
        saved_refs,
        [(copied, Some(PathBuf::from("Samples/Imported/kick.wav")))]
    );
}

#[test]
fn collect_all_takes_only_the_locations_it_is_told_to() {
    let dir = Dir::new();
    let in_lib = dir.0.join("Library/Samples/lib.wav");
    let tone = dir.file("Elsewhere/kick.wav");
    let (_e, mut s) = session();
    s.library = Some(lib(&dir));
    write_tone(&in_lib, 1.0, 0.4);
    write_tone(&tone, 1.0, 0.4);
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    place(&mut s, t, &in_lib, 4, 16 * PPQ);
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    s.collect_all_and_save(CollectOptions {
        user_library: false,
        other_projects: true,
        elsewhere: true,
    })
    .unwrap();
    let got = refs(&s);
    assert!(got.contains(&in_lib), "the User Library stays");
    assert!(got.contains(&set.parent().unwrap().join("Samples/Imported/kick.wav")));
}

#[test]
fn collecting_two_files_with_one_name_keeps_both() {
    let dir = Dir::new();
    let (a, b) = (dir.file("A/x.wav"), dir.file("B/x.wav"));
    write_tone(&a, 1.0, 0.4);
    write_tone(&b, 1.0, 0.2);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &a, 4, 0);
    place(&mut s, t, &b, 4, 16 * PPQ);
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    s.collect_all_and_save(CollectOptions::default()).unwrap();
    let imported = set.parent().unwrap().join("Samples/Imported");
    assert!(imported.join("x.wav").exists());
    assert!(imported.join("x 2.wav").exists());
    let mut got = refs(&s);
    got.sort();
    assert_eq!(got.len(), 2);
    assert_ne!(got[0], got[1], "each clip still has its own audio");
}

#[test]
fn a_collected_project_round_trips_without_its_sources() {
    let dir = Dir::new();
    let tone = dir.file("Source/tone.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    s.collect_all_and_save(CollectOptions::default()).unwrap();
    arrive(&mut s);
    std::fs::remove_dir_all(dir.0.join("Source")).unwrap();
    let moved = dir.0.join("Somewhere Else");
    std::fs::rename(set.parent().unwrap(), &moved).unwrap();
    let (_e2, mut other) = session();
    other.load(&moved.join("Song.rlive")).unwrap();
    assert!(other.missing_files().is_empty());
    arrive(&mut other);
    let out = dir.file("rendered.wav");
    other.export_arrangement(&out).unwrap();
    let audio = rlive_devices::load_wav(&out).unwrap();
    assert!(audio.left.iter().any(|x| x.abs() > 0.1), "it still sounds");
}

#[test]
fn collect_needs_a_saved_set() {
    let (_e, mut s) = session();
    assert!(s.collect_all_and_save(CollectOptions::default()).is_err());
}

#[test]
fn a_missing_file_is_listed_and_does_not_stop_a_collect() {
    let dir = Dir::new();
    let gone = dir.0.join("gone.wav");
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    place(&mut s, t, &gone, 4, 0);
    s.save_as(&dir.file("Song.rlive")).unwrap();
    let report = s.collect_all_and_save(CollectOptions::default()).unwrap();
    assert_eq!(report.missing, [gone]);
    assert_eq!(report.copied, 0);
}

// ------------------------------------------------------------- Projects and the Library

fn song_in(dir: &Dir, s: &mut Session, name: &str, sample: &Path) -> PathBuf {
    let t = audio_track(s);
    place(s, t, sample, 4, 0);
    s.save_as(&dir.file(name)).unwrap()
}

#[test]
fn a_project_report_counts_every_set_in_it_and_finds_unused_files() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let set = song_in(&dir, &mut s, "Song.rlive", &tone);
    let root = set.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(root.join("Samples/Recorded")).unwrap();
    std::fs::write(root.join("Samples/Recorded/stale.wav"), b"x").unwrap();
    let report = s.scope_report(&ManagedScope::Project(root.clone()));
    assert_eq!(report.documents, 1);
    assert_eq!(report.external[0].files, 1);
    assert_eq!(report.unused[0].category, "Recorded");
    assert_eq!(report.unused[0].files.len(), 1);
    // A file in use by the open Set isn't unused, even before it's saved.
    let recorded = root.join("Samples/Recorded/stale.wav");
    write_tone(&recorded, 1.0, 0.3);
    let t = s.project.tracks[0].id;
    place(&mut s, t, &recorded, 4, 16 * PPQ);
    let again = s.scope_report(&ManagedScope::Project(root));
    assert!(again.unused.is_empty(), "{:?}", again.unused);
}

#[test]
fn collecting_a_project_rewrites_its_other_sets_and_keeps_a_backup() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let set = song_in(&dir, &mut s, "Song.rlive", &tone);
    let root = set.parent().unwrap().to_path_buf();
    let second = root.join("Second.rlive");
    s.save_as(&second).unwrap();
    s.load(&set).unwrap();
    let report = s
        .collect_scope(
            &ManagedScope::Project(root.clone()),
            CollectOptions::default(),
        )
        .unwrap();
    assert_eq!(report.copied, 1);
    let collected = root.join("Samples/Imported/a.wav");
    for path in [&set, &second] {
        let p = Project::load(path).unwrap();
        let mut seen = Vec::new();
        p.for_each_file_ref(&mut |f: &FileRef| seen.push(f.path.clone()));
        assert_eq!(seen, std::slice::from_ref(&collected), "{path:?}");
    }
    assert!(root.join("Backup").read_dir().unwrap().count() >= 1);
}

#[test]
fn collecting_the_library_gathers_samples_into_it() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/hit.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    s.library = Some(lib(&dir));
    let clip = audio_clip(&tone, 4);
    let path = dir.0.join("Library/Clips/hit.rlclip");
    rlive_browser::preset::save_clip_file(&path, &rlive_browser::preset::ClipFile::new(clip))
        .unwrap();
    s.collect_scope(&ManagedScope::UserLibrary, CollectOptions::default())
        .unwrap();
    let file = rlive_browser::preset::load_clip_file(&path).unwrap();
    let ClipContent::Audio { file: f, .. } = &file.clip.content else {
        panic!()
    };
    assert_eq!(f.path, lib(&dir).imported_samples_dir().join("hit.wav"));
}

#[test]
fn repairing_a_project_fixes_files_moved_around_inside_it() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let set = s.save_as(&dir.file("Song.rlive")).unwrap();
    let root = set.parent().unwrap().to_path_buf();
    let tone = root.join("Samples/a.wav");
    std::fs::create_dir_all(tone.parent().unwrap()).unwrap();
    write_tone(&tone, 1.0, 0.4);
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 4, 0);
    s.save(&set).unwrap();
    s.new_project();
    let new_home = root.join("Samples/Sorted/Drums/a.wav");
    std::fs::create_dir_all(new_home.parent().unwrap()).unwrap();
    std::fs::rename(&tone, &new_home).unwrap();
    let report = s
        .repair_scope(
            &ManagedScope::Project(root.clone()),
            &SearchOptions {
                folder: Some(root.clone()),
                rescan: true,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.relinked, [(tone, new_home.clone())]);
    let p = Project::load(&set).unwrap();
    let mut seen = Vec::new();
    p.for_each_file_ref(&mut |f: &FileRef| seen.push(f.path.clone()));
    assert_eq!(seen, [new_home]);
}

#[test]
fn trashing_unused_files_leaves_the_rest() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (a, b) = (dir.file("a.wav"), dir.file("b.wav"));
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    assert_eq!(s.trash_unused(std::slice::from_ref(&a)).unwrap(), 1);
    assert!(!a.exists());
    assert!(b.exists());
}

// ------------------------------------------------------------- clips

fn poly_midi(s: &mut Session) -> TrackId {
    s.add_track(TrackKind::Midi, None)
}

#[test]
fn a_clip_saved_for_the_browser_keeps_its_devices_and_copies_its_sample() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.add_builtin(ChainRef::Track(t), "rlive.reverb");
    s.set_clip(t, 0, Some(audio_clip(&tone, 4)));
    let saved = s
        .save_clip_to(ClipRef::slot(t, 0), &dir.file("Clips"), "Loop", true)
        .unwrap();
    let root = dir.0.join("Clips/Loop Project");
    assert_eq!(saved, root.join("Loop.rlclip"));
    assert!(root.join(MARKER).exists());
    let file = rlive_browser::preset::load_clip_file(&saved).unwrap();
    assert_eq!(file.devices.len(), 1, "the track's devices come along");
    let ClipContent::Audio { file: f, .. } = &file.clip.content else {
        panic!()
    };
    assert_eq!(f.path, root.join("Samples/Imported/a.wav"));
    assert!(f.path.exists());
}

#[test]
fn a_clip_saved_without_copying_keeps_the_original_path() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&tone, 4)));
    let saved = s
        .save_clip_to(ClipRef::slot(t, 0), &dir.file("Clips"), "Loop", false)
        .unwrap();
    let file = rlive_browser::preset::load_clip_file(&saved).unwrap();
    let ClipContent::Audio { file: f, .. } = &file.clip.content else {
        panic!()
    };
    assert_eq!(f.path, tone);
    assert!(!dir.0.join("Clips/Loop Project").exists());
}

#[test]
fn a_bare_clip_from_an_older_file_still_loads() {
    let dir = Dir::new();
    let path = dir.file("old.rlclip");
    let clip = audio_clip(Path::new("/x.wav"), 4);
    std::fs::write(&path, ron::to_string(&clip).unwrap()).unwrap();
    let got = rlive_browser::preset::load_clip_file(&path).unwrap();
    assert_eq!(got.clip.name, "x");
    assert!(got.devices.is_empty());
}

#[test]
fn saving_a_preset_can_leave_its_samples_where_they_are() {
    let dir = Dir::new();
    let tone = dir.file("Elsewhere/a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let library = lib(&dir);
    s.library = Some(library.clone());
    let t = poly_midi(&mut s);
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.simpler");
    let id = s.devices(chain).last().unwrap().id;
    s.set_device_sample(chain, id, tone.clone());
    s.samples.wait_for([tone.as_path()]);
    s.poll_samples();
    s.collect_on_export = false;
    let path = s.save_device_preset(chain, id, Some("Off")).unwrap();
    let mut p = rlive_browser::preset::Preset::load(&path).unwrap();
    assert_eq!(p.file_refs_mut()[0].path, tone, "Never: the original path");
    s.collect_on_export = true;
    let path = s.save_device_preset(chain, id, Some("On")).unwrap();
    let mut p = rlive_browser::preset::Preset::load(&path).unwrap();
    assert!(
        p.file_refs_mut()[0]
            .path
            .starts_with(library.imported_samples_dir()),
        "Always: copied"
    );
}

#[test]
fn a_saved_clip_brings_its_devices_only_to_an_empty_track() {
    let dir = Dir::new();
    let tone = dir.file("a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.add_builtin(ChainRef::Track(t), "rlive.reverb");
    s.set_clip(t, 0, Some(audio_clip(&tone, 4)));
    let saved = s
        .save_clip_to(ClipRef::slot(t, 0), &dir.file("Clips"), "Loop", false)
        .unwrap();
    let file = rlive_browser::preset::load_clip_file(&saved).unwrap();
    let empty = audio_track(&mut s);
    s.place_clip_file(empty, 0, file.clone());
    assert_eq!(
        s.devices(ChainRef::Track(empty)).len(),
        1,
        "devices come along"
    );
    let busy = audio_track(&mut s);
    s.add_builtin(ChainRef::Track(busy), "rlive.delay");
    s.place_clip_file(busy, 0, file);
    assert_eq!(
        s.devices(ChainRef::Track(busy)).len(),
        1,
        "the track's own devices stay"
    );
    assert!(s.clip(busy, 0).is_some());
}

// ------------------------------------------------------------- merging and exporting Sets

#[test]
fn merging_a_set_adds_its_tracks_and_leaves_its_returns_behind() {
    let dir = Dir::new();
    let tone = dir.file("a.wav");
    write_tone(&tone, 1.0, 0.4);
    let (_e, mut other) = session();
    let t = audio_track(&mut other);
    other.project.track_mut(t).unwrap().name = "Merged".into();
    other.add_builtin(ChainRef::Track(t), "rlive.reverb");
    other.add_track(TrackKind::Return, None);
    other.set_clip(t, 0, Some(audio_clip(&tone, 4)));
    let set = other.save_as(&dir.file("Other.rlive")).unwrap();
    let (_e2, mut s) = session();
    let own = s.project.tracks.len();
    let added = s.merge_set(&set).unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(s.project.tracks.len(), own + 1);
    assert_eq!(s.project.returns.len(), 0, "returns are left out");
    let t2 = s.project.track(added[0]).unwrap();
    assert_eq!(t2.name, "Merged");
    assert_ne!(added[0], t, "with ids of its own");
    assert_eq!(s.devices(ChainRef::Track(added[0])).len(), 1);
    assert!(s.clip(added[0], 0).is_some());
    arrive(&mut s);
}

#[test]
fn selected_session_clips_export_as_a_new_set_with_only_those_clips() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let t = poly_midi(&mut s);
    let u = poly_midi(&mut s);
    let v = poly_midi(&mut s);
    let sig = s.project.time_signature;
    s.set_clip(t, 0, Some(Clip::new_midi("A", [1, 1, 1], 1, sig)));
    s.set_clip(t, 1, Some(Clip::new_midi("skip", [1, 1, 1], 1, sig)));
    s.set_clip(u, 0, Some(Clip::new_midi("B", [1, 1, 1], 1, sig)));
    s.set_clip(v, 0, Some(Clip::new_midi("skip", [1, 1, 1], 1, sig)));
    let path = s
        .export_clips_as_set(&[(t, 0), (u, 0)], &dir.file("Clips.rlive"))
        .unwrap();
    assert_eq!(path, dir.0.join("Clips Project/Clips.rlive"));
    let p = Project::load(&path).unwrap();
    assert_eq!(p.tracks.len(), 2);
    assert_eq!(p.tracks[0].clips[0].as_ref().unwrap().name, "A");
    assert!(p.tracks[0].clips[1].is_none());
    assert_eq!(p.tracks[1].clips[0].as_ref().unwrap().name, "B");
}

// ------------------------------------------------------------- Export

fn bar_tone(s: &mut Session, dir: &Dir, name: &str, level: f32) -> TrackId {
    let tone = dir.file(name);
    write_tone(&tone, 4.0, level);
    let t = audio_track(s);
    place(s, t, &tone, 8, 0);
    t
}

fn options() -> ExportOptions {
    ExportOptions {
        range: ExportRange::Timeline {
            start: 0,
            length: 4 * PPQ,
        },
        sample_rate: 48_000,
        pcm: Some(PcmFormat {
            kind: ExportType::Wav,
            depth: BitDepth::Bits32,
            dither: Dither::None,
        }),
        ..ExportOptions::default()
    }
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |p, s| p.max(s.abs()))
}

#[test]
fn every_bit_depth_is_what_it_says() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    for (depth, bits) in [
        (BitDepth::Bits16, 16),
        (BitDepth::Bits24, 24),
        (BitDepth::Bits32, 32),
    ] {
        for kind in [ExportType::Wav, ExportType::Aiff, ExportType::Flac] {
            let mut o = options();
            o.pcm = Some(PcmFormat {
                kind,
                depth,
                dither: Dither::None,
            });
            let out = dir.file(&format!("d{bits}{kind:?}.{}", kind.extension()));
            let files = s.export_now(&o, &out).unwrap();
            assert_eq!(files, std::slice::from_ref(&out));
            let stored = if kind == ExportType::Wav {
                hound::WavReader::open(&out).unwrap().spec().bits_per_sample as u32
            } else {
                probe(&out).unwrap().bits_per_sample.unwrap_or(0)
            };
            assert_eq!(stored, bits, "{kind:?} {bits}");
            let audio = decode(&out).unwrap();
            assert!(peak(&audio.left) > 0.4);
        }
    }
}

#[test]
fn a_chosen_rate_changes_the_length_and_not_the_pitch() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let beat_seconds = 60.0 / s.project.tempo;
    let seconds = 4.0 * beat_seconds;
    for rate in [22_050u32, 44_100, 96_000] {
        let mut o = options();
        o.sample_rate = rate;
        let out = dir.file(&format!("r{rate}.wav"));
        s.export_now(&o, &out).unwrap();
        let audio = decode(&out).unwrap();
        assert_eq!(audio.sample_rate, rate, "{rate}");
        let want = (seconds * rate as f64) as isize;
        assert!(
            (audio.frames() as isize - want).abs() < 4,
            "{rate}: {}",
            audio.frames()
        );
        // Zero crossings in the middle of the file: 440 Hz, at whatever rate.
        let body = &audio.left[rate as usize / 2..rate as usize];
        let crossings = body
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((crossings as i64 - 440).abs() <= 2, "{rate}: {crossings}");
        assert!(peak(body) > 0.4, "{rate}");
    }
}

#[test]
fn normalize_brings_the_loudest_peak_to_full_scale() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.25);
    let mut o = options();
    o.normalize = true;
    let out = dir.file("n.wav");
    s.export_now(&o, &out).unwrap();
    let audio = decode(&out).unwrap();
    assert!(
        (peak(&audio.left) - 1.0).abs() < 1e-3,
        "{}",
        peak(&audio.left)
    );
}

#[test]
fn dither_only_happens_on_the_way_to_a_lower_depth() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let tone = dir.file("t.wav");
    // A constant small level: dither shows up as noise on it.
    let dc = vec![0.1f32; (4.0 * SR) as usize];
    rlive_devices::write_wav(&tone, &dc, &dc, SR as u32).unwrap();
    let t = audio_track(&mut s);
    place(&mut s, t, &tone, 8, 0);
    let spread = |s: &mut Session, depth, dither, name: &str| {
        let mut o = options();
        o.pcm = Some(PcmFormat {
            kind: ExportType::Wav,
            depth,
            dither,
        });
        let out = dir.file(name);
        s.export_now(&o, &out).unwrap();
        let audio = decode(&out).unwrap();
        let body = &audio.left[5_000..20_000];
        let mean = body.iter().sum::<f32>() / body.len() as f32;
        body.iter().map(|x| (x - mean).abs()).fold(0.0, f32::max)
    };
    let dry32 = spread(&mut s, BitDepth::Bits32, Dither::Triangular, "a.wav");
    let none32 = spread(&mut s, BitDepth::Bits32, Dither::None, "a2.wav");
    assert_eq!(dry32, none32, "no dither at 32 bits");
    let plain16 = spread(&mut s, BitDepth::Bits16, Dither::None, "b.wav");
    assert!(plain16 < 1e-4, "no noise without dither: {plain16}");
    let tri16 = spread(&mut s, BitDepth::Bits16, Dither::Triangular, "c.wav");
    assert!(tri16 > 3e-5, "dithered: {tri16}");
}

#[test]
fn convert_to_mono_writes_one_channel() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let mut o = options();
    o.mono = true;
    let out = dir.file("m.wav");
    s.export_now(&o, &out).unwrap();
    assert_eq!(probe(&out).unwrap().channels, 1);
}

#[test]
fn create_analysis_file_writes_one_next_to_the_audio() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let mut o = options();
    o.create_analysis = true;
    let out = dir.file("a.wav");
    s.export_now(&o, &out).unwrap();
    assert!(rlive_audio_io::analysis::sidecar_path(&out).exists());
}

#[test]
fn start_and_length_choose_the_rendered_stretch() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let mut o = options();
    o.range = ExportRange::Timeline {
        start: 2 * PPQ,
        length: PPQ,
    };
    let out = dir.file("x.wav");
    s.export_now(&o, &out).unwrap();
    let audio = decode(&out).unwrap();
    assert_eq!(audio.frames(), (60.0 / s.project.tempo * 48_000.0) as usize);
}

#[test]
fn all_individual_tracks_writes_one_file_each_all_the_same_length() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (a, b) = (
        bar_tone(&mut s, &dir, "one.wav", 0.5),
        bar_tone(&mut s, &dir, "two.wav", 0.25),
    );
    s.project.track_mut(a).unwrap().name = "Lead".into();
    s.project.track_mut(b).unwrap().name = "Bass".into();
    s.project.track_mut(b).unwrap().volume_db = -6.0;
    s.rebuild();
    arrive(&mut s);
    let mut o = options();
    o.rendered = RenderedTrack::AllIndividual;
    let out = dir.file("Song.wav");
    let files = s.export_now(&o, &out).unwrap();
    let names: Vec<_> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Song Lead.wav", "Song Bass.wav", "Song Main.wav"]);
    let audio: Vec<_> = files.iter().map(|f| decode(f).unwrap()).collect();
    assert!(audio.iter().all(|a| a.frames() == audio[0].frames()));
    let lead = peak(&audio[0].left);
    let bass = peak(&audio[1].left);
    assert!((lead - 0.5).abs() < 0.03, "{lead}");
    assert!(
        (bass - 0.25 * 10f32.powf(-6.0 / 20.0)).abs() < 0.03,
        "post-fader: {bass}"
    );
    assert!(peak(&audio[2].left) > lead, "the Main output has both");
}

#[test]
fn a_midi_track_with_nothing_on_it_is_left_out_but_effects_alone_export_silence() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let empty = poly_midi(&mut s);
    let chain = ChainRef::Track(empty);
    while let Some(id) = s.devices(chain).first().map(|d| d.id) {
        s.remove_device(chain, id);
    }
    let fx = poly_midi(&mut s);
    let chain = ChainRef::Track(fx);
    while let Some(id) = s.devices(chain).first().map(|d| d.id) {
        s.remove_device(chain, id);
    }
    s.add_builtin(chain, "rlive.reverb");
    s.project.track_mut(empty).unwrap().name = "Empty".into();
    s.project.track_mut(fx).unwrap().name = "Fx".into();
    let mut o = options();
    o.rendered = RenderedTrack::AllIndividual;
    let files = s.export_now(&o, &dir.file("S.wav")).unwrap();
    let names: Vec<_> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(!names.iter().any(|n| n.contains("Empty")));
    let silent = decode(
        files
            .iter()
            .find(|p| p.to_string_lossy().contains("Fx"))
            .unwrap(),
    )
    .unwrap();
    assert!(peak(&silent.left) < 1e-4);
}

#[test]
fn a_single_named_track_and_a_selection_render_only_those_tracks() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let a = bar_tone(&mut s, &dir, "one.wav", 0.5);
    let b = bar_tone(&mut s, &dir, "two.wav", 0.5);
    let mut o = options();
    o.rendered = RenderedTrack::One(a);
    let files = s.export_now(&o, &dir.file("x.wav")).unwrap();
    assert_eq!(files.len(), 1);
    assert!((peak(&decode(&files[0]).unwrap().left) - 0.5).abs() < 0.03);
    o.rendered = RenderedTrack::Selected(vec![b, a]);
    assert_eq!(s.export_now(&o, &dir.file("y.wav")).unwrap().len(), 2);
}

#[test]
fn include_return_and_main_effects_adds_sends_and_the_main_chain() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let t = bar_tone(&mut s, &dir, "one.wav", 0.25);
    let ret = s.add_track(TrackKind::Return, None);
    s.project.track_mut(t).unwrap().sends = vec![rlive_project::TrackSend {
        to: ret,
        level_db: 0.0,
        enabled: true,
    }];
    s.rebuild();
    arrive(&mut s);
    let mut o = options();
    o.rendered = RenderedTrack::One(t);
    let dry = decode(&s.export_now(&o, &dir.file("dry.wav")).unwrap()[0]).unwrap();
    o.include_returns_and_main = true;
    let wet = decode(&s.export_now(&o, &dir.file("wet.wav")).unwrap()[0]).unwrap();
    assert!((peak(&dry.left) - 0.25).abs() < 0.02, "{}", peak(&dry.left));
    assert!(
        peak(&wet.left) > 0.4,
        "the return adds to it: {}",
        peak(&wet.left)
    );
}

#[test]
fn a_return_track_on_its_own_is_what_is_sent_to_it() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let t = bar_tone(&mut s, &dir, "one.wav", 0.5);
    let ret = s.add_track(TrackKind::Return, None);
    s.project.track_mut(t).unwrap().sends = vec![rlive_project::TrackSend {
        to: ret,
        level_db: -6.0,
        enabled: true,
    }];
    s.rebuild();
    arrive(&mut s);
    let mut o = options();
    o.rendered = RenderedTrack::One(ret);
    let audio = decode(&s.export_now(&o, &dir.file("r.wav")).unwrap()[0]).unwrap();
    let want = 0.5 * 10f32.powf(-6.0 / 20.0);
    assert!(
        (peak(&audio.left) - want).abs() < 0.03,
        "{}",
        peak(&audio.left)
    );
}

#[test]
fn render_as_loop_wraps_the_tail_into_the_start() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    // A burst near the end, then an echo past the end of the file.
    let hit = dir.file("hit.wav");
    let beat = 60.0 / s.project.tempo;
    let mut left = vec![0.0f32; (4.0 * beat * SR) as usize];
    let at = (3.75 * beat * SR) as usize;
    for (i, x) in left[at..at + 400].iter_mut().enumerate() {
        *x = 0.8 * (1.0 - i as f32 / 400.0);
    }
    rlive_devices::write_wav(&hit, &left, &left, SR as u32).unwrap();
    let t = audio_track(&mut s);
    place(&mut s, t, &hit, 4, 0);
    let chain = ChainRef::Track(t);
    s.add_builtin(chain, "rlive.reverb");
    arrive(&mut s);
    let mut o = options();
    let out = dir.file("plain.wav");
    s.export_now(&o, &out).unwrap();
    let plain = decode(&out).unwrap();
    o.render_as_loop = true;
    let out = dir.file("loop.wav");
    s.export_now(&o, &out).unwrap();
    let looped = decode(&out).unwrap();
    assert_eq!(looped.frames(), plain.frames(), "no longer than the render");
    let start = (0.5 * beat * SR) as usize;
    assert!(
        peak(&plain.left[..start]) < 1e-4,
        "plain: nothing before the end"
    );
    assert!(
        peak(&looped.left[..start]) > 1e-3,
        "the tail comes round: {}",
        peak(&looped.left[..start])
    );
}

#[test]
fn only_the_session_clips_playing_now_are_in_the_render() {
    let dir = Dir::new();
    let (mut e, mut s) = session();
    let timeline = bar_tone(&mut s, &dir, "a.wav", 0.5);
    let session_tone = dir.file("b.wav");
    write_tone(&session_tone, 4.0, 0.5);
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(&session_tone, 4)));
    arrive(&mut s);
    let mut o = options();
    o.rendered = RenderedTrack::One(t);
    let idle = decode(&s.export_now(&o, &dir.file("i.wav")).unwrap()[0]).unwrap();
    assert!(peak(&idle.left) < 1e-4, "not playing, so not heard");
    s.launch_clip(t, 0);
    let (mut l, mut r) = ([0.0f32; 512], [0.0f32; 512]);
    for _ in 0..60 {
        e.process_stereo(&mut l, &mut r);
        s.idle();
    }
    assert_eq!(s.track_shared[&t].playing_slot.load(Ordering::Relaxed), 0);
    let on = decode(&s.export_now(&o, &dir.file("o.wav")).unwrap()[0]).unwrap();
    assert!(peak(&on.left) > 0.3, "now it is");
    o.rendered = RenderedTrack::One(timeline);
    let other = decode(&s.export_now(&o, &dir.file("p.wav")).unwrap()[0]).unwrap();
    assert!(peak(&other.left) > 0.3, "the timeline track still plays");
}

#[test]
fn pcm_and_mp3_can_be_written_together_or_mp3_on_its_own() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let mut o = options();
    o.sample_rate = 44_100;
    o.mp3 = true;
    let out = dir.file("Song.wav");
    let files = s.export_now(&o, &out).unwrap();
    assert_eq!(files, [dir.0.join("Song.wav"), dir.0.join("Song.mp3")]);
    assert!(files.iter().all(|f| f.exists()));
    let mp3 = decode(&files[1]).unwrap();
    assert_eq!(mp3.sample_rate, 44_100);
    assert!(peak(&mp3.left) > 0.3);
    o.pcm = None;
    let solo = s.export_now(&o, &dir.file("Solo.wav")).unwrap();
    assert_eq!(solo, [dir.0.join("Solo.mp3")]);
    o.mp3 = false;
    assert!(s.export_now(&o, &dir.file("None.wav")).is_err());
}

#[test]
fn an_export_writes_nothing_until_it_finishes_and_can_be_cancelled() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    let out = dir.file("x.wav");
    s.start_export(&options(), &out).unwrap();
    assert!(s.render_status().is_some());
    assert!(!out.exists());
    s.cancel_render();
    assert!(!out.exists());
}

#[test]
fn export_outputs_lists_the_files_before_rendering() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    bar_tone(&mut s, &dir, "t.wav", 0.5);
    s.project.tracks[0].name = "A/B?".into();
    let mut o = options();
    o.rendered = RenderedTrack::AllIndividual;
    let list = s.export_outputs(&o, &dir.file("S.wav"));
    assert_eq!(
        list,
        [dir.0.join("S A-B-.wav"), dir.0.join("S Main.wav")],
        "track names become safe file names"
    );
}

// ------------------------------------------------------------- MIDI files

#[test]
fn a_clip_exports_to_a_midi_file_that_reads_back_the_same() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let t = poly_midi(&mut s);
    let mut clip = Clip::new_midi("Riff", [1, 2, 3], 1, s.project.time_signature);
    clip.notes_mut().unwrap().extend([
        NoteData::new(0, 480, 60, 1.0),
        NoteData::new(480, 240, 64, 0.5),
        NoteData::new(960, 960, 67, 0.25),
    ]);
    s.set_clip(t, 0, Some(clip));
    let out = dir.file("riff.mid");
    s.export_midi_clip(ClipRef::slot(t, 0), &out).unwrap();
    let file = rlive_project::midi_file::parse(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(file.division, 960);
    assert_eq!(file.name, "Riff");
    assert!((file.tempo.unwrap() - s.project.tempo).abs() < 0.01);
    let notes: Vec<_> = file
        .notes
        .iter()
        .map(|n| (n.start, n.length, n.key))
        .collect();
    assert_eq!(notes, [(0, 480, 60), (480, 240, 64), (960, 960, 67)]);
    assert_eq!(file.notes[1].velocity, 64);
}

#[test]
fn an_audio_clip_can_not_export_as_midi() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let t = audio_track(&mut s);
    s.set_clip(t, 0, Some(audio_clip(Path::new("/x.wav"), 4)));
    assert!(s
        .export_midi_clip(ClipRef::slot(t, 0), &dir.file("x.mid"))
        .is_err());
}

#[test]
fn a_midi_file_imports_to_a_slot_or_the_timeline() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let t = poly_midi(&mut s);
    let mut clip = Clip::new_midi("Source", [1, 2, 3], 1, s.project.time_signature);
    clip.notes_mut()
        .unwrap()
        .push(NoteData::new(240, 480, 72, 0.5));
    s.set_clip(t, 2, Some(clip));
    let out = dir.file("In.mid");
    s.export_midi_clip(ClipRef::slot(t, 2), &out).unwrap();
    s.set_clip(t, 2, None);
    let r = s
        .import_midi_file(&out, t, MidiImportTarget::Slot(1))
        .unwrap();
    assert_eq!(r, ClipRef::slot(t, 1));
    assert_eq!(s.clip(t, 1).unwrap().name, "In");
    assert_eq!(s.clip(t, 1).unwrap().notes()[0].key, 72);
    let r = s
        .import_midi_file(&out, t, MidiImportTarget::Timeline(4 * PPQ))
        .unwrap();
    let lane = s.clip_at(r).unwrap();
    assert_eq!(lane.notes()[0].start, 240);
    assert_eq!(s.project.track(t).unwrap().arrangement[0].start, 4 * PPQ);
    let audio = audio_track(&mut s);
    assert!(s
        .import_midi_file(&out, audio, MidiImportTarget::Slot(0))
        .is_err());
}

// ------------------------------------------------------------- Slice to MIDI

/// A clip of `path`, warped at the Set's tempo so a beat is a beat.
fn warped_clip(s: &Session, path: &Path, seconds: f64) -> Clip {
    let mut clip = audio_clip(path, 1);
    let tempo = s.project.tempo;
    let ticks = seconds * tempo / 60.0 * PPQ as f64;
    clip.length = ticks.round() as u64;
    if let ClipContent::Audio { warp, .. } = &mut clip.content {
        warp.enabled = true;
        warp.markers = vec![
            rlive_project::WarpMarker {
                seconds: 0.0,
                tick: 0.0,
            },
            rlive_project::WarpMarker {
                seconds,
                tick: ticks,
            },
        ];
    }
    clip
}

fn slice_clip(s: &mut Session, hits: Option<&[f64]>, dir: &Dir) -> (TrackId, PathBuf) {
    let path = dir.file("loop.wav");
    match hits {
        Some(h) => write_hits(&path, 2.0, h),
        None => write_tone(&path, 2.0, 0.5),
    }
    let t = audio_track(s);
    (t, path)
}

fn simplers_of(s: &Session, chain: ChainRef, rack: rlive_core::DeviceId) -> Vec<Device> {
    let DeviceKind::Rack { chains, .. } = &s.rack(chain, rack).unwrap().kind else {
        panic!("not a rack");
    };
    chains.iter().map(|c| c.devices[0].clone()).collect()
}

fn rack_of(s: &Session, track: TrackId) -> rlive_core::DeviceId {
    let chain = ChainRef::Track(track);
    let d = s.devices(chain)[0].clone();
    assert!(matches!(
        d.kind,
        DeviceKind::Rack {
            kind: RackKind::Drum,
            ..
        }
    ));
    d.id
}

#[test]
fn slicing_by_transients_cuts_at_the_onsets_and_plays_them_in_order() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let hits = [0.0, 0.25, 0.5, 1.0, 1.5];
    let (t, path) = slice_clip(&mut s, Some(&hits), &dir);
    let clip = warped_clip(&s, &path, 2.0);
    s.set_clip(t, 0, Some(clip));
    arrive(&mut s);
    assert!(s.samples.transients(&path).is_some(), "the analysis ran");
    let track = s
        .slice_to_new_track(
            ClipRef::slot(t, 0),
            &SliceOptions {
                by: SliceBy::Transients,
                preserve_warped: false,
                preset: None,
            },
        )
        .unwrap()
        .unwrap();
    assert_ne!(track, t);
    let rack = rack_of(&s, track);
    let chain = ChainRef::Track(track);
    let simplers = simplers_of(&s, chain, rack);
    assert_eq!(simplers.len(), 5, "one slice per hit");
    let clip = s.clip(track, 0).unwrap();
    let notes: Vec<_> = clip.notes().iter().map(|n| n.key).collect();
    assert_eq!(notes, [36, 37, 38, 39, 40], "a staircase");
    for (i, d) in simplers.iter().enumerate() {
        assert_eq!(d.sample(0).map(|f| f.path.clone()), Some(path.clone()));
        let start = d.params[&si_start()];
        assert!((start - hits[i] / 2.0).abs() < 0.02, "slice {i}: {start}");
        let end = d.params[&si_end()];
        let want = hits.get(i + 1).copied().unwrap_or(2.0) / 2.0;
        assert!((end - want).abs() < 0.02, "slice {i}: {end}");
    }
    let tempo = s.project.tempo;
    for (n, hit) in clip.notes().iter().zip(hits) {
        let tick = hit * tempo / 60.0 * PPQ as f64;
        assert!((n.start as f64 - tick).abs() < 960.0, "{} {tick}", n.start);
    }
}

fn si_start() -> u32 {
    rlive_devices::devices::b10::simpler::START_FLAG
}

fn si_end() -> u32 {
    rlive_devices::devices::b10::simpler::END_FLAG
}

#[test]
fn slicing_by_beats_divides_the_loop_region_exactly() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, None, &dir);
    let mut clip = warped_clip(&s, &path, 2.0);
    clip.looped = true;
    clip.loop_start = PPQ;
    clip.length = 2 * PPQ;
    s.set_clip(t, 0, Some(clip));
    let plan = s
        .slice_plan(ClipRef::slot(t, 0), SliceBy::Grid(PPQ / 2))
        .unwrap();
    assert_eq!(plan.region, (PPQ, 3 * PPQ));
    assert_eq!(plan.count(), 4, "two beats at a half beat each");
    assert_eq!(plan.ticks, [0, PPQ / 2, PPQ, 3 * PPQ / 2, 2 * PPQ]);
    let seconds_per_tick = 60.0 / s.project.tempo / PPQ as f64;
    for (i, sec) in plan.seconds.iter().enumerate() {
        let want = (PPQ + i as u64 * PPQ / 2) as f64 * seconds_per_tick;
        assert!((sec - want).abs() < 1e-3, "{i}: {sec} {want}");
    }
}

#[test]
fn slicing_by_warp_markers_cuts_at_the_markers() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, None, &dir);
    let mut clip = warped_clip(&s, &path, 2.0);
    if let ClipContent::Audio { warp, .. } = &mut clip.content {
        warp.markers.insert(
            1,
            rlive_project::WarpMarker {
                seconds: 0.5,
                tick: 400.0,
            },
        );
    }
    s.set_clip(t, 0, Some(clip));
    let plan = s
        .slice_plan(ClipRef::slot(t, 0), SliceBy::WarpMarkers)
        .unwrap();
    assert_eq!(plan.count(), 2);
    assert!((plan.seconds[1] - 0.5).abs() < 1e-3);
}

#[test]
fn slicing_refuses_more_than_a_rack_can_hold() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, None, &dir);
    let clip = warped_clip(&s, &path, 2.0);
    let ticks = clip.length;
    s.set_clip(t, 0, Some(clip));
    let by = SliceBy::Grid(ticks / 129);
    let plan = s.slice_plan(ClipRef::slot(t, 0), by).unwrap();
    assert!(plan.count() > 128, "{}", plan.count());
    let n = s.project.tracks.len();
    let err = s
        .slice_to_new_track(
            ClipRef::slot(t, 0),
            &SliceOptions {
                by,
                preserve_warped: false,
                preset: None,
            },
        )
        .unwrap_err();
    assert!(err.contains("128"), "{err}");
    assert_eq!(s.project.tracks.len(), n, "nothing was made");
    assert_ne!(
        s.slice_plan(ClipRef::slot(t, 0), SliceBy::Grid(ticks / 100))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn many_slices_fit_in_the_keys_there_are() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, None, &dir);
    let clip = warped_clip(&s, &path, 2.0);
    let ticks = clip.length;
    s.set_clip(t, 0, Some(clip));
    let by = SliceBy::Grid(ticks / 127);
    let track = s
        .slice_to_new_track(
            ClipRef::slot(t, 0),
            &SliceOptions {
                by,
                preserve_warped: false,
                preset: None,
            },
        )
        .unwrap()
        .unwrap();
    let notes = s.clip(track, 0).unwrap().notes().to_vec();
    assert!(notes.iter().all(|n| n.key <= 127));
    assert!(notes.len() > 100);
}

#[test]
fn without_preserve_warped_timing_a_slice_is_raw_audio_at_its_own_speed() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, Some(&[0.0, 1.0]), &dir);
    // Warped to twice the length: a beat of clip is half the audio it would be.
    let mut clip = warped_clip(&s, &path, 2.0);
    if let ClipContent::Audio { warp, .. } = &mut clip.content {
        warp.markers[1].tick *= 2.0;
    }
    clip.length *= 2;
    s.set_clip(t, 0, Some(clip));
    arrive(&mut s);
    let track = s
        .slice_to_new_track(
            ClipRef::slot(t, 0),
            &SliceOptions {
                by: SliceBy::Transients,
                preserve_warped: false,
                preset: None,
            },
        )
        .unwrap()
        .unwrap();
    let notes = s.clip(track, 0).unwrap().notes().to_vec();
    let want = 1.0 * s.project.tempo / 60.0 * PPQ as f64;
    assert_eq!(notes.len(), 2);
    assert!(
        (notes[1].start as f64 - want).abs() < 960.0,
        "the raw timing: {} {want}",
        notes[1].start
    );
}

#[test]
fn preserve_warped_timing_renders_the_warped_audio_first() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, Some(&[0.0, 1.0]), &dir);
    let clip = warped_clip(&s, &path, 2.0);
    s.set_clip(t, 0, Some(clip));
    arrive(&mut s);
    let n = s.project.tracks.len();
    let out = s
        .slice_to_new_track(
            ClipRef::slot(t, 0),
            &SliceOptions {
                by: SliceBy::Transients,
                preserve_warped: true,
                preset: None,
            },
        )
        .unwrap();
    assert_eq!(out, None, "the track arrives when the render is done");
    s.finish_render().unwrap();
    assert_eq!(s.project.tracks.len(), n + 1);
    let track = s.project.tracks.last().unwrap().id;
    let rack = rack_of(&s, track);
    let simplers = simplers_of(&s, ChainRef::Track(track), rack);
    let file = simplers[0].sample(0).unwrap().path.clone();
    assert_ne!(file, path, "a new, warped file");
    assert!(file.exists());
    assert_eq!(s.clip(track, 0).unwrap().notes().len(), 2);
}

#[test]
fn a_new_track_from_slicing_has_macros_on_every_simpler() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, Some(&[0.0, 1.0]), &dir);
    let clip = warped_clip(&s, &path, 2.0);
    s.set_clip(t, 0, Some(clip));
    arrive(&mut s);
    let track = s
        .slice_to_new_track(ClipRef::slot(t, 0), &SliceOptions::default())
        .unwrap()
        .unwrap();
    let DeviceKind::Rack { macros, .. } = &s.devices(ChainRef::Track(track))[0].kind else {
        panic!();
    };
    assert_eq!(macros[0].name, "Attack");
    assert_eq!(macros[0].mappings.len(), 2, "one per slice");
}

#[test]
fn a_slicing_preset_shapes_every_slice_and_its_macros() {
    let dir = Dir::new();
    let (_e, mut s) = session();
    let (t, path) = slice_clip(&mut s, Some(&[0.0, 1.0]), &dir);
    let clip = warped_clip(&s, &path, 2.0);
    s.set_clip(t, 0, Some(clip));
    arrive(&mut s);
    let tmpl_t = poly_midi(&mut s);
    let chain = ChainRef::Track(tmpl_t);
    let rack = s.add_rack(chain, RackKind::Drum, "Mine").unwrap();
    let pad = s.new_pad_chain(chain, rack, 36, "Pad").unwrap();
    let at = chain.child(rack, pad).unwrap();
    let simpler = Device::builtin("rlive.simpler", "Simpler");
    let id = simpler.id;
    s.add_device(at, None, simpler);
    s.set_param(at, id, rlive_devices::devices::b10::simpler::VOLUME, -3.0);
    assert!(s.map_macro(chain, rack, 2, param_ref(id)));
    let preset = rlive_browser::preset::Preset::of_device(s.rack(chain, rack).unwrap(), "Mine");
    let file = dir.file("Slicing/Mine.rlpreset");
    preset.save(&file).unwrap();
    let track = s
        .slice_to_new_track(
            ClipRef::slot(t, 0),
            &SliceOptions {
                preset: Some(file),
                ..SliceOptions::default()
            },
        )
        .unwrap()
        .unwrap();
    let chain = ChainRef::Track(track);
    let rack = rack_of(&s, track);
    let simplers = simplers_of(&s, chain, rack);
    assert_eq!(simplers.len(), 2);
    assert_eq!(
        simplers[1].params[&rlive_devices::devices::b10::simpler::VOLUME],
        -3.0
    );
    let DeviceKind::Rack { macros, .. } = &s.devices(chain)[0].kind else {
        panic!();
    };
    assert_eq!(macros[2].mappings.len(), 2, "the preset's Macro on both");
}

fn param_ref(id: rlive_core::DeviceId) -> rlive_core::param::ParamRef {
    rlive_core::param::ParamRef {
        owner: rlive_core::param::ParamOwner::Device(id),
        id: rlive_core::ParamId(rlive_devices::devices::b10::simpler::A_RELEASE),
    }
}

#[test]
fn only_audio_clips_can_be_sliced() {
    let (_e, mut s) = session();
    let t = poly_midi(&mut s);
    s.set_clip(
        t,
        0,
        Some(Clip::new_midi("x", [1, 2, 3], 1, s.project.time_signature)),
    );
    assert!(s
        .slice_plan(ClipRef::slot(t, 0), SliceBy::Transients)
        .is_err());
}
