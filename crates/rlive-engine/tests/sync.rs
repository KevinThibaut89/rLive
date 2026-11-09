//! MIDI Clock, timecode, Tempo Follower and Link against the engine, with injected messages.

use rlive_core::sync::{FrameRate, SyncMessage as M, Timecode};
use rlive_core::time::{BarMap, TempoMap, TempoPoint, TimeMaps};
use rlive_engine::sync::{
    ClockPort, ExternalSync, HwSync, LinkClock, LinkHolder, LinkState, SyncConfig,
};
use rlive_engine::*;
use rlive_testkit::assert_no_alloc;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn send(h: &mut EngineHandle, c: TransportCmd) {
    assert!(h.send(EngineCommand::Transport(c)).is_ok());
}

fn run(engine: &mut Engine, frames: usize) {
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    let mut left = frames;
    while left > 0 {
        let n = left.min(BLOCK);
        engine.process_stereo(&mut l[..n], &mut r[..n]);
        left -= n;
    }
}

fn clock_rig(ports: &[ClockPort]) -> (Engine, EngineHandle, rtrb::Consumer<HwSync>) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let out = h.take_hw_sync().unwrap();
    let mut config = SyncConfig::default();
    for p in ports {
        config.add_output(*p);
    }
    send(&mut h, TransportCmd::SetSync(Box::new(config)));
    (engine, h, out)
}

fn drain(out: &mut rtrb::Consumer<HwSync>) -> Vec<HwSync> {
    std::iter::from_fn(|| out.pop().ok()).collect()
}

fn clock_frames(msgs: &[HwSync], port: u16) -> Vec<u64> {
    msgs.iter()
        .filter(|m| m.port == port && m.bytes[0] == 0xf8)
        .map(|m| m.frame)
        .collect()
}

#[test]
fn clock_goes_out_on_the_sample_twenty_four_to_the_beat() {
    let (mut engine, mut h, mut out) = clock_rig(&[ClockPort {
        port: 2,
        delay: 0.0,
    }]);
    send(&mut h, TransportCmd::SetTempo(120.0));
    send(&mut h, TransportCmd::Play);
    run(&mut engine, 48_000);
    let msgs = drain(&mut out);
    assert_eq!(msgs[0].bytes[0], 0xfa, "Start comes first");
    assert_eq!(msgs[1].bytes[0], 0xf8);
    let clocks = clock_frames(&msgs, 2);
    assert_eq!(clocks.len(), 48);
    for (n, frame) in clocks.iter().enumerate() {
        assert_eq!(
            *frame as f64,
            n as f64 * 1000.0,
            "120 BPM is 1000 frames a clock"
        );
    }
}

#[test]
fn clock_follows_a_ramp_to_within_a_sample() {
    let tempo = TempoMap::new([
        TempoPoint::curved(0.0, 90.0, 0.6),
        TempoPoint::new(3840.0, 180.0),
    ]);
    let (mut engine, mut h, mut out) = clock_rig(&[ClockPort {
        port: 0,
        delay: 0.0,
    }]);
    send(
        &mut h,
        TransportCmd::SetTimeMaps(Arc::new(TimeMaps {
            tempo: tempo.clone(),
            bars: BarMap::default(),
        })),
    );
    send(&mut h, TransportCmd::Play);
    run(
        &mut engine,
        (tempo.tick_to_seconds(3840.0) * SR) as usize + 2000,
    );
    let clocks = clock_frames(&drain(&mut out), 0);
    for (n, frame) in clocks.iter().take(96).enumerate() {
        let due = tempo.tick_to_seconds(n as f64 * 40.0) * SR;
        assert!(
            (*frame as f64 - due).abs() <= 1.0,
            "clock {n}: {frame} vs {due}"
        );
    }
}

#[test]
fn stopping_sends_stop_and_seeking_says_where_the_song_went() {
    let (mut engine, mut h, mut out) = clock_rig(&[ClockPort {
        port: 0,
        delay: 0.0,
    }]);
    send(&mut h, TransportCmd::Play);
    run(&mut engine, BLOCK);
    drain(&mut out);
    send(&mut h, TransportCmd::Seek(3840.0));
    run(&mut engine, BLOCK);
    let msgs = drain(&mut out);
    let spp = msgs
        .iter()
        .find(|m| m.bytes[0] == 0xf2)
        .expect("a position");
    assert_eq!((spp.bytes[1], spp.bytes[2]), (16, 0));
    send(&mut h, TransportCmd::Stop);
    run(&mut engine, BLOCK);
    assert!(drain(&mut out).iter().any(|m| m.bytes[0] == 0xfc));
}

#[test]
fn a_start_from_the_middle_says_continue_after_the_position() {
    let (mut engine, mut h, mut out) = clock_rig(&[ClockPort {
        port: 0,
        delay: 0.0,
    }]);
    send(&mut h, TransportCmd::Seek(1920.0));
    send(&mut h, TransportCmd::Play);
    run(&mut engine, BLOCK);
    let msgs = drain(&mut out);
    assert_eq!(msgs[0].bytes[0], 0xf2);
    assert_eq!(msgs[1].bytes[0], 0xfb);
    assert!(!msgs.iter().any(|m| m.bytes[0] == 0xfa));
}

#[test]
fn sync_delay_moves_each_port_on_its_own() {
    let early = ClockPort {
        port: 0,
        delay: -0.005,
    };
    let late = ClockPort {
        port: 1,
        delay: 0.010,
    };
    let (mut engine, mut h, mut out) = clock_rig(&[early, late]);
    send(&mut h, TransportCmd::SetTempo(120.0));
    send(&mut h, TransportCmd::Play);
    run(&mut engine, 24_000);
    let msgs = drain(&mut out);
    let a = clock_frames(&msgs, 0);
    // A clock read 5 ms ahead sits 240 frames sooner than the grid; the first goes at once.
    assert_eq!(a[0], 0);
    assert_eq!(a[1] as i64, 1000 - 240);
    let b: Vec<&HwSync> = msgs
        .iter()
        .filter(|m| m.port == 1 && m.bytes[0] == 0xf8)
        .collect();
    assert_eq!(b[1].frame, 1000);
    assert!((b[1].offset - ((1000.0 - 768.0) / SR + 0.010)).abs() < 1e-9);
}

#[test]
fn emitting_clock_allocates_nothing() {
    let (mut engine, mut h, mut out) = clock_rig(&[ClockPort {
        port: 0,
        delay: 0.0,
    }]);
    send(&mut h, TransportCmd::Play);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for _ in 0..400 {
        assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
        while out.pop().is_ok() {}
    }
}

/// A clock source at `bpm`: its next message is due at `frame`, every `period` frames.
struct Host {
    period: f64,
    frame: f64,
}

impl Host {
    fn new(bpm: f64) -> Self {
        Self {
            period: SR * 60.0 / bpm / 24.0,
            frame: 0.0,
        }
    }

    /// Sends the clocks due before the end of the next block, and renders it.
    fn block(&mut self, engine: &mut Engine, tx: &mut MidiSender, clock_frame: &mut u64) {
        let end = *clock_frame + BLOCK as u64;
        while (self.frame as u64) < end {
            tx.send_sync_at(M::Clock, self.frame as u64);
            self.frame += self.period;
        }
        run(engine, BLOCK);
        *clock_frame = end;
    }
}

fn following() -> (Engine, EngineHandle, MidiSender) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let tx = h.take_midi_sender().unwrap();
    send(
        &mut h,
        TransportCmd::SetSync(Box::new(SyncConfig {
            external: ExternalSync::Clock { delay: 0.0 },
            ..SyncConfig::default()
        })),
    );
    (engine, h, tx)
}

#[test]
fn an_incoming_clock_sets_the_tempo_and_starts_on_time() {
    let (mut engine, h, mut tx) = following();
    let mut host = Host::new(132.0);
    let mut frame = 0;
    tx.send_sync_at(M::Start, 0);
    for _ in 0..400 {
        host.block(&mut engine, &mut tx, &mut frame);
    }
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    let tempo = h.shared.tempo.load();
    assert!((tempo - 132.0).abs() < 0.2, "tempo {tempo}");
    // Position: 400 blocks, against what the host has sent.
    let tick = h.shared.tick.load();
    let want = frame as f64 / SR * 132.0 / 60.0 * 960.0;
    assert!((tick - want).abs() < 30.0, "tick {tick} want {want}");
}

#[test]
fn the_song_follows_a_change_of_tempo_at_the_host() {
    let (mut engine, h, mut tx) = following();
    let mut host = Host::new(110.0);
    let mut frame = 0;
    tx.send_sync_at(M::Start, 0);
    for _ in 0..400 {
        host.block(&mut engine, &mut tx, &mut frame);
    }
    host.period = SR * 60.0 / 90.0 / 24.0;
    for _ in 0..1200 {
        host.block(&mut engine, &mut tx, &mut frame);
    }
    let tempo = h.shared.tempo.load();
    assert!((tempo - 90.0).abs() < 0.3, "tempo {tempo}");
}

#[test]
fn stop_stops_and_a_song_position_moves_the_song() {
    let (mut engine, h, mut tx) = following();
    let mut host = Host::new(120.0);
    let mut frame = 0;
    tx.send_sync_at(M::Start, 0);
    for _ in 0..100 {
        host.block(&mut engine, &mut tx, &mut frame);
    }
    tx.send_sync_at(M::Stop, frame);
    host.block(&mut engine, &mut tx, &mut frame);
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    // 32 sixteenths is two bars.
    tx.send_sync_at(M::SongPosition(32), frame);
    host.block(&mut engine, &mut tx, &mut frame);
    assert_eq!(h.shared.tick.load(), 7680.0);
}

#[test]
fn a_song_position_is_wrapped_into_the_loop() {
    let (mut engine, mut h, mut tx) = following();
    send(&mut h, TransportCmd::SetLoop(Some((0.0, 3840.0))));
    run(&mut engine, BLOCK);
    // Five bars in, in a one-bar loop, is at the start of it.
    tx.send_sync_at(M::SongPosition(80), 0);
    run(&mut engine, BLOCK);
    assert_eq!(h.shared.tick.load(), 0.0);
}

#[test]
fn external_clock_takes_over_a_tempo_map() {
    let (mut engine, mut h, mut tx) = following();
    send(
        &mut h,
        TransportCmd::SetTimeMaps(Arc::new(TimeMaps {
            tempo: TempoMap::new([TempoPoint::new(0.0, 80.0), TempoPoint::new(96000.0, 100.0)]),
            bars: BarMap::default(),
        })),
    );
    let mut host = Host::new(140.0);
    let mut frame = 0;
    tx.send_sync_at(M::Start, 0);
    for _ in 0..200 {
        host.block(&mut engine, &mut tx, &mut frame);
    }
    assert!((h.shared.tempo.load() - 140.0).abs() < 0.3);
}

fn timecode_rig(rate: Option<FrameRate>, offset: f64) -> (Engine, EngineHandle, MidiSender) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let tx = h.take_midi_sender().unwrap();
    send(
        &mut h,
        TransportCmd::SetSync(Box::new(SyncConfig {
            external: ExternalSync::Timecode {
                rate,
                offset,
                delay: 0.0,
            },
            ..SyncConfig::default()
        })),
    );
    (engine, h, tx)
}

/// Sends `seconds` of running timecode from `start`, a piece every quarter frame.
fn send_timecode(
    engine: &mut Engine,
    tx: &mut MidiSender,
    start: f64,
    seconds: f64,
    rate: FrameRate,
) -> u64 {
    let quarter = 1.0 / rate.fps() / 4.0;
    let mut sent = 0u64;
    let mut frame = 0u64;
    let total = (seconds * SR) as u64;
    while frame < total {
        // Everything due in the next block goes in ahead of rendering it.
        while (sent as f64 * quarter * SR) < (frame + BLOCK as u64) as f64 {
            let frame_no = sent / 8;
            let whole = (frame_no as f64 * 2.0 / rate.fps()) + start;
            let tc = Timecode::from_seconds(whole, rate);
            let piece = tc.quarter_frames()[(sent % 8) as usize];
            tx.send_sync_at(M::QuarterFrame(piece), (sent as f64 * quarter * SR) as u64);
            sent += 1;
        }
        run(engine, BLOCK);
        frame += BLOCK as u64;
    }
    frame
}

#[test]
fn running_timecode_drives_the_arrangement_with_the_offset_taken_off() {
    let (mut engine, h, mut tx) = timecode_rig(Some(FrameRate::Fps25), 10.0);
    // Running from 12 s against an offset of 10 s: the song runs from 2 s on.
    let frame = send_timecode(&mut engine, &mut tx, 12.0, 2.0, FrameRate::Fps25);
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    let tick = h.shared.tick.load();
    let want = (2.0 + frame as f64 / SR) * 1920.0;
    assert!((tick - want).abs() < 20.0, "tick {tick} want {want}");
}

#[test]
fn smpte_all_reads_the_rate_from_the_stream() {
    let (mut engine, h, mut tx) = timecode_rig(None, 0.0);
    let frame = send_timecode(&mut engine, &mut tx, 3.0, 1.0, FrameRate::Fps24);
    let tick = h.shared.tick.load();
    let want = (3.0 + frame as f64 / SR) * 1920.0;
    assert!((tick - want).abs() < 20.0, "tick {tick} want {want}");
}

#[test]
fn timecode_that_stops_stops_the_transport() {
    let (mut engine, h, mut tx) = timecode_rig(Some(FrameRate::Fps30), 0.0);
    send_timecode(&mut engine, &mut tx, 1.0, 0.5, FrameRate::Fps30);
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    run(&mut engine, 24_000);
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn a_full_frame_message_locates_without_running() {
    let (mut engine, h, mut tx) = timecode_rig(Some(FrameRate::Fps30), 0.0);
    let tc = Timecode::from_seconds(2.0, FrameRate::Fps30);
    tx.send_sync_at(M::FullFrame(tc), 0);
    run(&mut engine, BLOCK);
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(h.shared.tick.load(), 3840.0);
}

fn clicks(bpm: f64, frames: usize, sr: f64) -> Vec<f32> {
    let period = sr * 60.0 / bpm;
    let mut out = vec![0.0f32; frames];
    let mut at = 0.0;
    while (at as usize) < frames {
        for i in 0..(0.01 * sr) as usize {
            if let Some(s) = out.get_mut(at as usize + i) {
                *s = (1.0 - i as f32 / (0.01 * sr) as f32) * if i % 7 < 3 { 0.8 } else { -0.8 };
            }
        }
        at += period;
    }
    out
}

fn feed_input(engine: &mut Engine, input: &[f32]) {
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for chunk in input.chunks(BLOCK) {
        let n = chunk.len();
        engine.process(AudioIo {
            inputs: &[chunk],
            outputs: &mut [&mut l[..n], &mut r[..n]],
        });
    }
}

#[test]
fn tempo_follower_sets_the_tempo_and_stays_out_when_told() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        TransportCmd::SetSync(Box::new(SyncConfig {
            follower: Some(0),
            follower_drives: true,
            ..SyncConfig::default()
        })),
    );
    feed_input(&mut engine, &clicks(133.0, 48_000 * 12, SR));
    let heard = h.shared.follower_tempo.load();
    assert!((heard - 133.0).abs() < 0.6, "heard {heard}");
    assert!((h.shared.tempo.load() - heard).abs() < 1e-6);

    // Tempo Leaders hold the tempo: it goes on hearing, without driving.
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        TransportCmd::SetSync(Box::new(SyncConfig {
            follower: Some(0),
            follower_drives: false,
            ..SyncConfig::default()
        })),
    );
    feed_input(&mut engine, &clicks(133.0, 48_000 * 12, SR));
    assert_eq!(h.shared.tempo.load(), 120.0);
    assert!(h.shared.follower_tempo.load() > 0.0);
}

#[test]
fn the_follower_says_when_its_input_is_missing() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    send(
        &mut h,
        TransportCmd::SetSync(Box::new(SyncConfig {
            follower: Some(7),
            follower_drives: true,
            ..SyncConfig::default()
        })),
    );
    run(&mut engine, BLOCK);
    assert!(!h
        .shared
        .follower_connected
        .load(std::sync::atomic::Ordering::Relaxed));
}

#[derive(Default)]
struct LinkLog {
    state: LinkState,
    tempos: Vec<f64>,
    playing: Vec<bool>,
}

struct FakeLink(Arc<Mutex<LinkLog>>);

impl LinkClock for FakeLink {
    fn capture(&mut self, _quantum: f64, _latency: f64) -> LinkState {
        self.0.lock().unwrap().state
    }
    fn set_tempo(&mut self, bpm: f64) {
        let mut log = self.0.lock().unwrap();
        log.tempos.push(bpm);
        log.state.tempo = bpm;
    }
    fn set_playing(&mut self, playing: bool) {
        self.0.lock().unwrap().playing.push(playing);
    }
}

fn link_rig() -> (Engine, EngineHandle, Arc<Mutex<LinkLog>>) {
    let (engine, mut h) = create_engine(SR, BLOCK);
    let log = Arc::new(Mutex::new(LinkLog {
        state: LinkState {
            tempo: 120.0,
            ..LinkState::default()
        },
        ..LinkLog::default()
    }));
    send(
        &mut h,
        TransportCmd::SetLink(Some(Box::new(LinkHolder(Box::new(FakeLink(log.clone())))))),
    );
    (engine, h, log)
}

#[test]
fn a_tempo_from_link_overrides_a_map_and_a_local_one_is_told_to_link() {
    let (mut engine, mut h, log) = link_rig();
    send(
        &mut h,
        TransportCmd::SetTimeMaps(Arc::new(TimeMaps {
            tempo: TempoMap::new([TempoPoint::new(0.0, 90.0), TempoPoint::new(9600.0, 150.0)]),
            bars: BarMap::default(),
        })),
    );
    run(&mut engine, BLOCK);
    log.lock().unwrap().state.tempo = 133.0;
    run(&mut engine, BLOCK * 2);
    assert_eq!(h.shared.tempo.load(), 133.0);

    send(&mut h, TransportCmd::TellLinkTempo(101.0));
    run(&mut engine, BLOCK);
    assert_eq!(log.lock().unwrap().tempos, vec![101.0]);
    assert_eq!(h.shared.tempo.load(), 101.0);
}

#[test]
fn playing_with_a_link_peer_waits_for_the_phase() {
    let (mut engine, mut h, log) = link_rig();
    log.lock().unwrap().state.peers = 1;
    log.lock().unwrap().state.phase = 2.0;
    run(&mut engine, BLOCK);
    send(&mut h, TransportCmd::Play);
    run(&mut engine, BLOCK * 2);
    assert!(!h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        h.shared
            .link_peers
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    // The phase comes round: playback starts.
    log.lock().unwrap().state.phase = 0.01;
    run(&mut engine, BLOCK);
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(log.lock().unwrap().playing, vec![true]);
}

#[test]
fn link_stays_in_phase_while_playing() {
    let (mut engine, mut h, log) = link_rig();
    log.lock().unwrap().state.phase = 0.0;
    send(&mut h, TransportCmd::Seek(0.0));
    send(&mut h, TransportCmd::Play);
    // No peers: it starts at once.
    run(&mut engine, BLOCK);
    // A peer turns up a whole beat away: we jump to line up.
    {
        let mut l = log.lock().unwrap();
        l.state.peers = 1;
        l.state.phase = 2.0;
    }
    run(&mut engine, BLOCK);
    let tick = h.shared.tick.load();
    assert!((tick / 960.0 - 2.0).abs() < 0.05, "tick {tick}");
}

#[test]
fn count_in_is_ignored_under_link() {
    let (mut engine, mut h, _log) = link_rig();
    assert!(h
        .send(EngineCommand::Record(RecordCmd::SetCountIn(2)))
        .is_ok());
    send(&mut h, TransportCmd::Play);
    run(&mut engine, BLOCK);
    assert!(h.shared.playing.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(h.shared.count_in_left.load(), 0.0);
}

#[test]
fn overriding_the_tempo_is_undone_by_the_map_coming_back_and_allocates_nothing() {
    let (mut engine, mut h) = create_engine(SR, BLOCK);
    let map = TempoMap::new([TempoPoint::new(0.0, 90.0), TempoPoint::new(9600.0, 150.0)]);
    let maps = |t: &TempoMap| {
        Arc::new(TimeMaps {
            tempo: t.clone(),
            bars: BarMap::default(),
        })
    };
    send(&mut h, TransportCmd::SetTimeMaps(maps(&map)));
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    send(&mut h, TransportCmd::Seek(4800.0));
    send(&mut h, TransportCmd::OverrideTempo(Some(77.0)));
    assert_no_alloc(|| engine.process_stereo(&mut l, &mut r));
    assert_eq!(h.shared.tempo.load(), 77.0);
    send(&mut h, TransportCmd::SetTimeMaps(maps(&map)));
    run(&mut engine, BLOCK);
    assert_eq!(h.shared.tempo.load(), 77.0, "it holds over a new map");
    send(&mut h, TransportCmd::OverrideTempo(None));
    send(&mut h, TransportCmd::SetTimeMaps(maps(&map)));
    run(&mut engine, BLOCK);
    assert!((h.shared.tempo.load() - map.bpm_at(4800.0)).abs() < 1e-6);
}

#[test]
fn nudging_runs_time_faster_and_slower() {
    let tick_after = |ratio: f64| {
        let (mut engine, mut h) = create_engine(SR, BLOCK);
        send(&mut h, TransportCmd::SetTempo(120.0));
        send(&mut h, TransportCmd::SetNudge(ratio));
        send(&mut h, TransportCmd::Play);
        run(&mut engine, 48_000);
        h.shared.tick.load()
    };
    assert!((tick_after(1.0) - 1920.0).abs() < 1e-6);
    assert!((tick_after(1.05) - 2016.0).abs() < 1e-3);
    assert!((tick_after(0.9) - 1728.0).abs() < 1e-3);
}
