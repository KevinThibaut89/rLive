//! Tempo override and recording, curved tempo, the Tempo Leader and EXT rules, Tap and Looper.

use super::sync::SyncPorts;
use super::*;
use crate::settings::{MidiPortConfig, RemoteSettings, SyncKind, SyncSettings};
use rlive_core::envelope::segment_value;
use rlive_core::sync::FrameRate;
use rlive_core::{ParamRange, ViewWriter, PPQ};
use rlive_engine::sync::{ClockPort, ExternalSync};
use rlive_engine::{create_engine, Engine};
use rlive_project::{envelope_edit, Breakpoint, ClipContent, DeviceKind, WarpMarker};

const SR: f64 = 48_000.0;

fn session() -> (Engine, Session) {
    let (engine, handle) = create_engine(SR, 512);
    (
        engine,
        Session::new(handle, Project::new_default(), PluginCache::default()),
    )
}

fn render(engine: &mut Engine, s: &mut Session, frames: usize) {
    let frames = frames.next_multiple_of(512);
    rlive_testkit::render_chunks(frames, 512, |l, r| {
        engine.process_stereo(l, r);
        s.idle();
    });
}

fn tempo_now(s: &Session) -> f64 {
    s.engine.shared.tempo.load()
}

fn automated(s: &mut Session, points: Vec<Breakpoint>) {
    s.project.arrangement.tempo.points = points;
    s.sync_time_maps();
}

fn range() -> ParamRange {
    ParamRange {
        min: 60.0,
        max: 200.0,
        skew: 1.0,
        quantized: false,
    }
}

#[test]
fn moving_the_tempo_with_no_automation_just_sets_it() {
    let (mut engine, mut s) = session();
    render(&mut engine, &mut s, 512);
    s.set_tempo(93.5);
    render(&mut engine, &mut s, 512);
    assert_eq!(tempo_now(&s), 93.5);
    assert!(!s.tempo_overridden());
    assert!(!s.any_overridden());
}

#[test]
fn moving_the_tempo_overrides_the_automation_until_re_enabled() {
    let (mut engine, mut s) = session();
    automated(
        &mut s,
        vec![Breakpoint::new(0.0, 70.0), Breakpoint::new(7680.0, 150.0)],
    );
    render(&mut engine, &mut s, 512);
    assert!((tempo_now(&s) - 70.0).abs() < 0.1);

    s.play();
    render(&mut engine, &mut s, 4800);
    s.set_tempo(133.0);
    render(&mut engine, &mut s, 4800);
    assert_eq!(tempo_now(&s), 133.0, "the field wins over the map");
    assert!(s.tempo_overridden());
    assert!(s.any_overridden(), "Re-Enable Automation lights up");

    s.re_enable_all();
    render(&mut engine, &mut s, 512);
    assert!(!s.tempo_overridden());
    let tick = s.engine.shared.tick.load();
    let want = 70.0 + (150.0 - 70.0) * tick / 7680.0;
    assert!((tempo_now(&s) - want).abs() < 0.5, "back on the ramp");
}

#[test]
fn an_override_survives_a_fresh_map_from_an_edit() {
    let (mut engine, mut s) = session();
    automated(
        &mut s,
        vec![Breakpoint::new(0.0, 70.0), Breakpoint::new(7680.0, 150.0)],
    );
    s.set_tempo(101.0);
    s.sync_time_maps();
    render(&mut engine, &mut s, 512);
    assert_eq!(tempo_now(&s), 101.0);
}

#[test]
fn tempo_moves_are_recorded_into_the_arrangement_under_automation_arm() {
    let (mut engine, mut s) = session();
    render(&mut engine, &mut s, 512);
    let before = s.project.tempo;
    s.set_automation_arm(true);
    assert!(s.start_recording());
    render(&mut engine, &mut s, 9600);
    let moved_at = s.engine.shared.tick.load();
    s.set_tempo(150.0);
    render(&mut engine, &mut s, 9600);
    assert_eq!(tempo_now(&s), 150.0, "heard as it is moved");
    let again = s.engine.shared.tick.load();
    s.set_tempo(90.0);
    render(&mut engine, &mut s, 9600);
    s.stop();
    render(&mut engine, &mut s, 1024);

    let points = s.project.arrangement.tempo.points.clone();
    assert!(!points.is_empty(), "the moves were written down");
    let at = |t: f64| envelope_edit::value_at(&points, t, range()).unwrap();
    assert!((at(0.0) - before).abs() < 1e-6, "what came before holds");
    assert!((at(moved_at + 100.0) - 150.0).abs() < 1.0, "{points:?}");
    assert!((at(again + 100.0) - 90.0).abs() < 1.0, "{points:?}");
    assert!(
        !s.tempo_overridden(),
        "the pass is over, the automation plays"
    );
    s.play();
    render(&mut engine, &mut s, 512);
    assert!((tempo_now(&s) - at(0.0)).abs() < 1.0);
}

#[test]
fn nothing_is_recorded_without_the_arm() {
    let (mut engine, mut s) = session();
    assert!(s.start_recording());
    render(&mut engine, &mut s, 4800);
    s.set_tempo(160.0);
    render(&mut engine, &mut s, 4800);
    s.stop();
    render(&mut engine, &mut s, 1024);
    assert!(s.project.arrangement.tempo.points.is_empty());
}

#[test]
fn the_map_and_the_player_agree_on_a_curved_ramp() {
    let (mut engine, mut s) = session();
    let mut start = Breakpoint::new(0.0, 80.0);
    start.curve = 0.8;
    let points = vec![start, Breakpoint::new(9600.0, 190.0)];
    automated(&mut s, points.clone());
    let map = build::time_maps(&s.project).tempo;
    s.play();
    for _ in 0..200 {
        render(&mut engine, &mut s, 512);
        let tick = s.engine.shared.tick.load();
        if tick >= 9600.0 {
            break;
        }
        let from_map = map.bpm_at(tick);
        let x = tick / 9600.0;
        assert!((from_map - segment_value(80.0, 190.0, x, 0.8)).abs() < 1e-9);
        // What's drawn in the lane, by the function every envelope shares.
        let drawn = envelope_edit::value_at(&points, tick, range()).unwrap();
        assert!((drawn - from_map).abs() < 1e-6, "{tick}");
        assert!((tempo_now(&s) - from_map).abs() < 1e-9, "{tick}");
    }
}

#[test]
fn signature_markers_drive_the_bar_map() {
    let (mut engine, mut s) = session();
    s.project
        .arrangement
        .signatures
        .push(rlive_project::SignatureMarker {
            tick: 3840,
            signature: rlive_core::TimeSignature {
                numerator: 3,
                denominator: 4,
            },
        });
    s.sync_time_maps();
    render(&mut engine, &mut s, 512);
    let maps = build::time_maps(&s.project);
    assert_eq!(maps.bars.bar_at(3840.0).signature.numerator, 3);
    assert_eq!(maps.bars.ticks_per_bar_at(3840.0), 2880);
    assert_eq!(
        maps.bars.next_boundary(3841.0, rlive_core::Quantize::Bar),
        6720.0
    );
}

fn set_ports(s: &mut Session, ports: SyncPorts) {
    s.configure_sync(ports, &SyncSettings::default());
}

fn leader_clip(s: &mut Session) {
    let bpm = 90.0;
    let bars = 2;
    let seconds = bars as f64 * 4.0 * 60.0 / bpm;
    let mut content = ClipContent::audio("a.wav", 0.0);
    if let ClipContent::Audio { warp, .. } = &mut content {
        warp.enabled = true;
        warp.tempo_leader = true;
        warp.markers = vec![
            WarpMarker {
                seconds: 0.0,
                tick: 0.0,
            },
            WarpMarker {
                seconds,
                tick: (bars * 4 * PPQ) as f64,
            },
        ];
    }
    let clip = rlive_project::ArrangementClip::new(
        rlive_project::Clip {
            length: bars * 4 * PPQ,
            content,
            ..rlive_project::Clip::default()
        },
        3840,
    );
    let track = s.project.tracks[0].id;
    s.project.track_mut(track).unwrap().arrangement.push(clip);
    s.sync_time_maps();
}

#[test]
fn ext_and_follower_exclude_each_other_and_lock_the_field() {
    let (mut engine, mut s) = session();
    assert!(!s.external_sync_available(), "no source yet");
    s.set_external_sync(true);
    assert!(!s.external_sync());

    set_ports(
        &mut s,
        SyncPorts {
            source: Some(ExternalSync::Clock { delay: 0.0 }),
            outputs: Vec::new(),
            follower_channel: Some(0),
        },
    );
    s.set_external_sync(true);
    assert!(s.externally_timed());
    assert!(s.tempo_field_locked());
    s.set_tempo(99.0);
    assert_ne!(s.project.tempo, 99.0);

    s.set_tempo_follower(true);
    assert!(!s.external_sync(), "Follow turns EXT off");
    assert!(!s.externally_timed());
    assert!(!s.external_sync_available(), "and EXT can't be had");
    assert!(s.tempo_field_locked(), "the Follower has the tempo");

    s.set_tempo_follower(false);
    assert!(s.external_sync_available());
    assert!(!s.tempo_field_locked());
    render(&mut engine, &mut s, 512);
}

#[test]
fn ext_deactivates_the_lead_follow_toggle_and_leaders_override_the_follower() {
    let (_engine, mut s) = session();
    set_ports(
        &mut s,
        SyncPorts {
            source: Some(ExternalSync::Clock { delay: 0.0 }),
            outputs: Vec::new(),
            follower_channel: Some(0),
        },
    );
    leader_clip(&mut s);
    s.set_external_sync(true);
    let track = s.project.tracks[0].id;
    let id = s.project.track(track).unwrap().arrangement[0].id;
    s.arr_set_tempo_leader(&[(track, id)], false);
    assert!(s.project.has_tempo_leader(), "EXT holds the toggle off");
    s.set_external_sync(false);
    s.arr_set_tempo_leader(&[(track, id)], false);
    assert!(!s.project.has_tempo_leader());

    leader_clip(&mut s);
    s.set_tempo_follower(true);
    assert!(s.follower_active());
    assert!(
        !s.follower_drives(),
        "clip tempo leaders win over the Follower"
    );
    assert!(s.tempo_field_locked());
}

#[test]
fn tapping_sets_the_tempo_and_four_taps_start_the_transport_in_four_four() {
    let (mut engine, mut s) = session();
    render(&mut engine, &mut s, 512);
    let beat = 60.0 / 100.0;
    s.tap_tempo(0.0, true);
    assert!(!s.is_playing());
    s.tap_tempo(beat, true);
    assert!((s.project.tempo - 100.0).abs() < 1e-6);
    s.tap_tempo(2.0 * beat, true);
    render(&mut engine, &mut s, 512);
    assert!(!s.is_playing(), "not yet");
    s.tap_tempo(3.0 * beat, true);
    render(&mut engine, &mut s, 512);
    assert!(s.is_playing());
    assert!((s.project.tempo - 100.0).abs() < 1e-6);
}

#[test]
fn tapping_without_the_option_only_sets_the_tempo() {
    let (mut engine, mut s) = session();
    for i in 0..5 {
        s.tap_tempo(i as f64 * 0.5, false);
    }
    render(&mut engine, &mut s, 512);
    assert!(!s.is_playing());
    assert!((tempo_now(&s) - 120.0).abs() < 0.01);
}

#[test]
fn a_long_gap_starts_a_new_count() {
    let (_engine, mut s) = session();
    s.tap_tempo(0.0, false);
    s.tap_tempo(0.5, false);
    s.tap_tempo(10.0, false);
    assert!((s.project.tempo - 120.0).abs() < 1e-6);
}

#[test]
fn a_mapped_fine_tempo_moves_only_the_hundredths() {
    let (_engine, mut s) = session();
    s.set_tempo(127.0);
    s.write_mapped_param(super::remote::transport_fine(), 0.5);
    assert!((s.project.tempo - 127.5).abs() < 1e-9);
}

#[test]
fn a_looper_set_and_follow_request_sets_the_tempo_once() {
    let (mut engine, mut s) = session();
    let track = s.project.tracks[0].id;
    s.add_builtin(ChainRef::Track(track), "rlive.looper");
    let id = s
        .devices(ChainRef::Track(track))
        .iter()
        .find(
            |d| matches!(&d.kind, DeviceKind::Builtin { type_id, .. } if type_id == "rlive.looper"),
        )
        .unwrap()
        .id;
    render(&mut engine, &mut s, 512);
    let view = s.runtimes[&id].view.clone().unwrap();
    s.poll_looper_tempo();
    use rlive_devices::devices::b6::looper;
    let mut values = [0.0; looper::VIEW_LEN];
    values[looper::TEMPO_REQUEST_SLOT] = 102.0;
    ViewWriter::new(&view).publish(looper::VIEW, &values);
    s.poll_looper_tempo();
    assert_eq!(s.project.tempo, 102.0);

    // A later manual change isn't undone by the same request still being on show.
    s.set_tempo(120.0);
    s.poll_looper_tempo();
    assert_eq!(s.project.tempo, 120.0);
}

#[test]
fn ports_with_their_sync_switch_on_become_the_source_and_the_clock_outs() {
    let mut remote = RemoteSettings::default();
    remote.set_input(MidiPortConfig {
        sync: true,
        sync_kind: SyncKind::Timecode,
        mtc_rate: Some(FrameRate::Fps25),
        mtc_offset: 3.0,
        sync_delay_ms: 5.0,
        ..remote.input("In", None)
    });
    let mut out = remote.output("Out");
    out.sync = true;
    out.sync_delay_ms = -2.0;
    remote.set_output(out);
    let ports = remote.sync_ports(
        &["Other".into(), "In".into()],
        &["Silent".into(), "Out".into()],
        None,
        Some(4),
    );
    assert_eq!(
        ports.source,
        Some(ExternalSync::Timecode {
            rate: Some(FrameRate::Fps25),
            offset: 3.0,
            delay: 0.005,
        })
    );
    assert_eq!(
        ports.outputs,
        vec![ClockPort {
            port: 1,
            delay: -0.002
        }]
    );
    assert_eq!(ports.follower_channel, Some(4));
}

#[test]
fn a_new_leader_takes_the_tempo_back_from_the_follower() {
    let (mut engine, mut s) = session();
    set_ports(
        &mut s,
        SyncPorts {
            source: None,
            outputs: Vec::new(),
            follower_channel: Some(0),
        },
    );
    s.set_tempo_follower(true);
    render(&mut engine, &mut s, 512);
    // As if it had been following something.
    s.send(EngineCommand::Transport(TransportCmd::OverrideTempo(Some(
        77.0,
    ))));
    render(&mut engine, &mut s, 512);
    assert_eq!(tempo_now(&s), 77.0);
    leader_clip(&mut s);
    render(&mut engine, &mut s, 512);
    assert_eq!(tempo_now(&s), s.project.tempo, "the follower let go");
}
