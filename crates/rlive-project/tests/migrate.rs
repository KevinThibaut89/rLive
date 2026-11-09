//! Files written by old versions of rLive: `fixtures/` holds real ones, one directory per format.

use rlive_core::{PluginFormat, Quantize, TimeSignature};
use rlive_project::{
    Arrangement, ClipContent, Device, DeviceKind, FileRef, Project, ProjectError, TrackKind,
    FORMAT_VERSION,
};
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(version: &str, name: &str) -> String {
    fs::read_to_string(fixtures().join(version).join(format!("{name}.rlive"))).unwrap()
}

/// A fresh, empty directory unique to `test`.
fn temp_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rlive-migrate-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn file_refs_of(d: &Device) -> Option<&Vec<Option<FileRef>>> {
    match &d.kind {
        DeviceKind::Builtin { samples, .. } => Some(samples),
        _ => None,
    }
}

/// Every fixture of every format, as `(format directory, file name, text)`.
fn all_fixtures() -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for dir in fs::read_dir(fixtures()).unwrap() {
        let dir = dir.unwrap().path();
        let version = dir.file_name().unwrap().to_string_lossy().into_owned();
        for file in fs::read_dir(&dir).unwrap() {
            let file = file.unwrap().path();
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            found.push((version.clone(), name, fs::read_to_string(&file).unwrap()));
        }
    }
    assert!(found.len() >= 4, "the fixtures are missing: {found:?}");
    found
}

#[test]
fn every_fixture_of_every_version_loads_and_round_trips_exactly() {
    for (version, name, text) in all_fixtures() {
        let p = Project::from_ron(&text).unwrap_or_else(|e| panic!("{version}/{name}: {e}"));
        assert_eq!(p.version, FORMAT_VERSION, "{version}/{name}");
        let saved = p.to_ron().unwrap();
        let again = Project::from_ron(&saved).unwrap();
        assert_eq!(p, again, "{version}/{name}: model changed on reload");
        assert_eq!(
            saved,
            again.to_ron().unwrap(),
            "{version}/{name}: text changed on reload"
        );
    }
}

#[test]
fn a_v1_file_upgrades_and_saves_as_v2() {
    let dir = temp_dir("upgrade");
    let path = dir.join("song.rlive");
    fs::write(&path, fixture("v1", "default")).unwrap();
    let p = Project::load(&path).unwrap();
    p.save(&path).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        ron::from_str::<ron::Value>(&text)
            .unwrap()
            .into_rust::<VersionOnly>()
            .unwrap()
            .version,
        2
    );
    let again = Project::load(&path).unwrap();
    assert_eq!(p, again);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        text,
        "loading doesn't write"
    );
    fs::remove_dir_all(&dir).ok();
}

#[derive(serde::Deserialize)]
struct VersionOnly {
    version: u32,
}

#[test]
fn the_default_project_keeps_its_content() {
    let p = Project::from_ron(&fixture("v1", "default")).unwrap();
    assert_eq!(p.tracks.len(), 3);
    assert_eq!(p.scenes.len(), 8);
    assert_eq!(p.scenes[0].name, "1");
    assert_eq!(p.tracks[0].kind, TrackKind::Midi);
    assert_eq!(p.tracks[2].kind, TrackKind::Audio);
    let notes = p.tracks[0].clips[0].as_ref().unwrap().notes();
    assert_eq!(notes.len(), 6);
    assert_eq!(notes[0].key, 57);
    assert_eq!(notes[0].release, 0.5);
    assert_eq!(notes[0].probability, 1.0);
    assert!(!notes[0].muted);
    assert_eq!(p.master.kind, TrackKind::Master);
    assert!(p.master.clips.is_empty(), "the Master has no clip slots");
    assert!(p.returns.is_empty());
}

#[test]
fn the_master_gets_an_id_that_collides_with_nothing_in_the_file() {
    let p = Project::from_ron(&fixture("v1", "audio_and_device_state")).unwrap();
    let mut seen = std::collections::HashSet::new();
    rlive_project::walk_ids(&p, |id| {
        assert!(seen.insert(id), "id {id} appears twice");
    });
    assert!(seen.contains(&p.master.id.0));
}

#[test]
fn arrangement_loops_survive_the_upgrade() {
    let p = Project::from_ron(&fixture("v1", "arrangement_loops")).unwrap();
    assert_eq!(p.tempo, 128.0);
    assert_eq!(p.quantize, Quantize::Beat);
    assert_eq!(
        p.time_signature,
        TimeSignature {
            numerator: 7,
            denominator: 8
        }
    );
    assert!(p.metronome);
    assert_eq!(
        p.arrangement,
        Arrangement {
            loop_enabled: true,
            loop_start: 1920,
            loop_end: 9600,
            ..Arrangement::default()
        }
    );
    assert_eq!(p.scenes.len(), 9);
    assert_eq!(p.scenes[8].name, "Outro");
    let lane = &p.tracks[0].arrangement;
    let spans: Vec<_> = lane.iter().map(|c| (c.start, c.length, c.offset)).collect();
    assert!(lane.windows(2).all(|w| w[0].start <= w[1].start));
    assert!(spans.iter().any(|s| s.0 == 1200 && s.2 == 240), "{spans:?}");
    assert!(lane
        .iter()
        .any(|c| c.clip.name == "Looped" && c.clip.looped));
    assert!(lane
        .iter()
        .any(|c| c.clip.name == "One shot" && !c.clip.looped));
    let looped = lane.iter().find(|c| c.clip.name == "Looped").unwrap();
    assert_eq!(looped.clip.notes()[0].key, 67);
    assert_eq!(p.tracks[1].arrangement.len(), 3, "split around the overdub");
}

#[test]
fn audio_clips_and_device_state_survive_the_upgrade() {
    let p = Project::from_ron(&fixture("v1", "audio_and_device_state")).unwrap();
    let t = &p.tracks[0];
    assert_eq!(t.kind, TrackKind::Audio);
    assert_eq!(t.volume_db, -6.5);
    assert_eq!(t.pan, 0.25);
    assert!(t.solo);
    let clip = t.clips[1].as_ref().unwrap();
    match &clip.content {
        ClipContent::Audio { file, gain_db, .. } => {
            assert_eq!(file.path, Path::new("/samples/break 120.wav"));
            assert_eq!(*gain_db, -3.25);
        }
        other => panic!("{other:?}"),
    }
    assert!(clip.active);
    let eq = &t.devices[0];
    assert!(eq.bypass && !eq.expanded);
    assert_eq!(eq.params[&0], 3.5);
    assert_eq!(eq.params[&2], -12.0);
    let vocal = t
        .arrangement
        .iter()
        .find(|c| c.clip.name == "Vocal")
        .unwrap();
    assert!(!vocal.clip.looped);
    assert_eq!(vocal.start, 3840);

    let keys = &p.tracks[1];
    assert!(keys.mute && keys.arm);
    let refs = file_refs_of(&keys.devices[0]).unwrap();
    assert_eq!(refs.len(), 1, "the sample moves to slot 0");
    assert_eq!(
        refs[0].as_ref().unwrap().path,
        Path::new("/samples/piano C3.wav")
    );
    assert!(file_refs_of(&keys.devices[1]).unwrap().is_empty());
    assert_eq!(keys.devices[0].params[&4], 0.75);

    assert_eq!(p.master.kind, TrackKind::Master);
    assert_eq!(p.master.volume_db, -1.5);
    assert_eq!(p.master.devices.len(), 2);
    assert_eq!(p.master.devices[0].params[&0], 0.5);
}

#[test]
fn plugin_state_blobs_survive_the_upgrade() {
    let p = Project::from_ron(&fixture("v1", "plugin_state_blob")).unwrap();
    let blob = |len: usize, seed: u8| -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed))
            .collect()
    };
    let state_of = |d: &Device| match &d.kind {
        DeviceKind::Plugin { format, state, .. } => (*format, state.clone()),
        other => panic!("{other:?}"),
    };
    let t = &p.tracks[0];
    assert_eq!(
        state_of(&t.devices[0]),
        (PluginFormat::Clap, Some(blob(1024, 3)))
    );
    assert_eq!(
        state_of(&t.devices[1]),
        (PluginFormat::Vst3, Some(blob(31, 200)))
    );
    assert!(t.devices[1].bypass);
    assert_eq!(
        state_of(&p.master.devices[0]),
        (PluginFormat::AudioUnit, Some(Vec::new()))
    );
    assert_eq!(state_of(&p.master.devices[1]), (PluginFormat::Clap, None));
    assert_eq!(t.devices[0].params[&7], 0.33);
    match &p.tracks[0].devices[0].kind {
        DeviceKind::Plugin {
            path, plugin_id, ..
        } => {
            assert_eq!(path, Path::new("/plugins/Synth.clap"));
            assert_eq!(plugin_id, "com.example.synth");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn upgrading_writes_a_backup_of_the_original_bytes() {
    let dir = temp_dir("backup");
    let path = dir.join("Song.rlive");
    let original = fixture("v1", "audio_and_device_state");
    fs::write(&path, &original).unwrap();
    Project::load(&path).unwrap();
    let backup = dir.join("Song.v1.bak");
    assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_existing_backup_is_never_overwritten() {
    let dir = temp_dir("keep-backup");
    let path = dir.join("Song.rlive");
    fs::write(&path, fixture("v1", "default")).unwrap();
    let backup = dir.join("Song.v1.bak");
    fs::write(&backup, "the first backup").unwrap();
    Project::load(&path).unwrap();
    assert_eq!(fs::read_to_string(&backup).unwrap(), "the first backup");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_failed_backup_fails_the_load() {
    let dir = temp_dir("failed-backup");
    // 249 characters plus `.rlive` is a legal name; plus `.v1.bak` it is one over the limit.
    let path = dir.join(format!("{}.rlive", "x".repeat(249)));
    fs::write(&path, fixture("v1", "default")).unwrap();
    match Project::load(&path) {
        Err(ProjectError::Backup { .. }) => {}
        other => panic!("expected a backup error, got {other:?}"),
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_current_file_makes_no_backup() {
    let dir = temp_dir("no-backup");
    let path = dir.join("Song.rlive");
    Project::new_default().save(&path).unwrap();
    Project::load(&path).unwrap();
    assert!(!dir.join("Song.v2.bak").exists());
    assert!(!dir.join("Song.v1.bak").exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_from_a_newer_rlive_is_refused_without_a_backup() {
    let dir = temp_dir("too-new");
    let path = dir.join("Song.rlive");
    fs::write(&path, format!("(version: {})", FORMAT_VERSION + 1)).unwrap();
    assert!(matches!(Project::load(&path), Err(ProjectError::TooNew(_))));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_new_fields_are_empty_defaults_and_cost_nothing_on_disk() {
    let p = Project::from_ron(&fixture("v1", "default")).unwrap();
    assert!(p.tracks[0].sends.is_empty());
    assert!(p.tracks[0].automation.is_empty());
    assert!(p.arrangement.locators.is_empty());
    assert!(p.scenes[0].tempo.is_none());
    let saved = p.to_ron().unwrap();
    assert!(
        !saved.contains("sends"),
        "empty fields stay out of the file"
    );
    assert!(!saved.contains("automation"));
    assert!(!saved.contains("probability"));
}

#[test]
fn the_v2_schema_round_trips_when_it_is_used() {
    use rlive_core::param::{ClipParam, EnvTarget, ParamId, ParamOwner, ParamRef};
    use rlive_core::{GrooveId, TrackId};
    use rlive_project::*;

    let mut p = Project::new_default();
    let groove = GrooveId(77_001);
    p.groove_pool.grooves.push(Groove {
        id: groove,
        name: "MPC 16 Swing".into(),
        file: Some(FileRef::new("/grooves/mpc.agr")),
        base: 240,
        quantize: 10.0,
        timing: 75.0,
        random: 2.0,
        velocity: -20.0,
        pattern: Default::default(),
    });
    p.tuning = Some(Tuning {
        name: "19 EDO".into(),
        description: String::new(),
        cents: (1..=19).map(|i| i as f64 * 1200.0 / 19.0).collect(),
        reference: rlive_project::ScaleNote {
            octave: 3,
            degree: 0,
        },
        reference_hz: 440.0,
        lowest: rlive_project::ScaleNote {
            octave: 0,
            degree: 0,
        },
        note_names: Vec::new(),
        source: String::new(),
        link: String::new(),
    });
    p.midi_mappings.push(MidiMapping {
        channel: 0,
        source: MidiSource::Cc {
            number: 7,
            encoding: CcEncoding::RelativeTwosComplement,
        },
        target: MapTarget::Param(ParamRef {
            owner: ParamOwner::Master,
            id: ParamId(0),
        }),
        min: -96.0,
        max: 6.0,
    });
    p.key_mappings.push(KeyMapping {
        key: "A".into(),
        target: MapTarget::Scene(3),
        min: 0.0,
        max: 1.0,
        toggle: false,
    });
    p.arrangement.locators.push(Locator {
        tick: 3840,
        name: "Chorus".into(),
        info: String::new(),
    });
    p.arrangement.signatures.push(SignatureMarker {
        tick: 7680,
        signature: TimeSignature {
            numerator: 3,
            denominator: 4,
        },
    });
    p.arrangement.tempo.points.push(Breakpoint::new(0.0, 100.0));
    p.arrangement
        .linked_tracks
        .push(vec![p.tracks[0].id, p.tracks[1].id]);
    p.scenes[0].tempo = Some(133.0);
    p.scenes[0].color = Some([1, 2, 3]);
    p.scenes[0].follow.enabled = true;
    p.scenes[0].follow.action_a = FollowAction::Jump(5);

    let ret = Track::new("A Reverb", TrackKind::Return, PALETTE[0], 0);
    let ret_id = ret.id;
    p.returns.push(ret);
    let group = Track::new("Group", TrackKind::Group, PALETTE[1], p.scenes.len());
    let group_id = group.id;
    p.tracks.push(group);

    let t = &mut p.tracks[0];
    t.group = Some(group_id);
    t.sends.push(TrackSend {
        to: ret_id,
        level_db: -12.0,
        enabled: true,
    });
    t.routing = Routing {
        input: Route::External(ExternalRoute::Midi {
            port: Some("Keys".into()),
            channel: Some(2),
        }),
        output: Route::Track {
            track: TrackId(77_002),
            device: None,
        },
        monitor: Monitor::In,
        tap: Tap::PreFx,
    };
    t.crossfade = Crossfade::B;
    t.pan_mode = PanMode::Split {
        left: -1.0,
        right: 0.5,
    };
    t.delay = TrackDelay::Ms(-3.5);
    t.automation.push(Envelope {
        target: EnvTarget::Param(ParamRef {
            owner: ParamOwner::Device(t.devices[0].id),
            id: ParamId(4),
        }),
        points: vec![Breakpoint {
            time: 480.0,
            value: 0.25,
            curve: -0.5,
        }],
    });
    t.take_lanes.push(TakeLane {
        name: "Take 1".into(),
        color: None,
        clips: Vec::new(),
    });
    t.ui.folded = true;
    t.ui.automation_lanes.push(EnvTarget::MidiCtrl(11));

    let clip = t.clips[0].as_mut().unwrap();
    clip.groove = Some(groove);
    clip.start = 120;
    clip.loop_start = 240;
    clip.end = Some(3000);
    clip.signature = Some(TimeSignature {
        numerator: 3,
        denominator: 4,
    });
    clip.scale.enabled = true;
    clip.scale.root = 5;
    clip.launch.mode = LaunchMode::Gate;
    clip.launch.quantize = Some(Quantize::None);
    clip.launch.legato = true;
    clip.launch.velocity_amount = 0.5;
    clip.fades.fade_in = 24;
    clip.envelopes.push(ClipEnvelope {
        envelope: Envelope {
            target: EnvTarget::Clip(ClipParam::Gain),
            points: vec![Breakpoint::new(0.0, 0.5)],
        },
        mode: EnvMode::Automation,
        loop_region: Some(EnvLoop::new(0, 960)),
    });
    let notes = clip.notes_mut().unwrap();
    notes[0].release = 0.25;
    notes[0].probability = 0.75;
    notes[0].vel_range = -0.1;
    notes[0].muted = true;
    notes[0].expr = Some(Box::new(NoteExpression {
        pitch: vec![Breakpoint::new(0.0, 0.0), Breakpoint::new(120.0, 2.0)],
        slide: vec![Breakpoint::new(0.0, 0.5)],
        pressure: vec![Breakpoint::new(60.0, 0.75)],
    }));

    let mut audio = Clip::new_midi("Audio", [9, 9, 9], 1, p.time_signature);
    audio.content = ClipContent::Audio {
        file: FileRef {
            path: "/a.wav".into(),
            relative: Some("Samples/a.wav".into()),
            size: Some(1234),
            hash: Some("blake3:ab".into()),
        },
        gain_db: -6.0,
        transpose: -7,
        detune_cents: 12.5,
        reverse: true,
        ram: true,
        hi_q: false,
        edge_fade: false,
        warp: Warp {
            enabled: true,
            mode: WarpMode::ComplexPro,
            markers: vec![WarpMarker {
                seconds: 1.5,
                tick: 1920.0,
            }],
            params: WarpParams {
                preserve: BeatsPreserve::Grid(240),
                beats_loop: BeatsLoop::BackAndForth,
                ..WarpParams::default()
            },
            ..Warp::default()
        },
    };
    p.tracks[2].clips[0] = Some(audio);

    let mut rack = Device::rack(RackKind::Drum, "Drums");
    let mut pad = Chain::new("Kick");
    pad.zones.drum = Some(DrumPad {
        receive: Some(36),
        play: 60,
        choke: Some(1),
    });
    pad.zones.key = Zone {
        low: 36,
        high: 36,
        fade_low: 30,
        fade_high: 40,
    };
    pad.mix.volume_db = -3.0;
    pad.mix.solo = true;
    pad.devices.push(Device::builtin("rlive.sampler", "Kick"));
    let mut nested = Device::rack(RackKind::Audio, "FX");
    let mut chain = Chain::new("Dry");
    chain.devices.push(Device::builtin("rlive.reverb", "Verb"));
    if let DeviceKind::Rack { chains, .. } = &mut nested.kind {
        chains.push(chain);
    }
    pad.devices.push(nested);
    let mut rack_return = Chain::new("Return");
    rack_return.mix.output = Some(ret_id);
    if let DeviceKind::Rack {
        chains,
        returns,
        macros,
        variations,
        ..
    } = &mut rack.kind
    {
        chains.push(pad);
        returns.push(rack_return);
        macros.push(Macro {
            name: "Macro 1".into(),
            value: 64.0,
            mappings: vec![MacroMapping {
                target: ParamRef {
                    owner: ParamOwner::Chain(rlive_core::ChainId(1)),
                    id: ParamId(0),
                },
                min: 1.0,
                max: 0.0,
            }],
            exclude_random: true,
            exclude_variations: false,
            ..Macro::default()
        });
        variations.push(Variation {
            name: "Bright".into(),
            values: vec![10.0],
        });
    }
    p.master.devices.push(rack);
    if let DeviceKind::Builtin { samples, state, .. } = &mut p.tracks[1].devices[0].kind {
        samples.push(None);
        samples.push(Some(FileRef::new("/b.wav")));
        *state = Some("(osc: 3)".into());
    }

    let text = p.to_ron().unwrap();
    let loaded = Project::from_ron(&text).unwrap();
    assert_eq!(p, loaded);
    assert_eq!(text, loaded.to_ron().unwrap());
}
