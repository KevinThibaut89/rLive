//! Renders a short Juno-106 demo to a WAV file and reports the real-time cost.
//!
//! `cargo run -p rlive-devices --release --example juno106_demo -- demo.wav`

use rlive_core::{MidiEvent, MidiMessage, Processor, TransportInfo};
use rlive_devices::juno106::{p, Juno106};
use rlive_testkit::Harness;
use std::time::Instant;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

struct Note {
    start: f64,
    end: f64,
    key: u8,
}

fn chord(start: f64, end: f64, keys: &[u8]) -> Vec<Note> {
    keys.iter().map(|&key| Note { start, end, key }).collect()
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "juno106_demo.wav".into());
    let mut j = Juno106::new();
    j.activate(SR, BLOCK);
    for (id, v) in [
        (p::SAW, 1.0),
        (p::PULSE, 1.0),
        (p::PWM_MODE, 0.0),
        (p::PWM, 6.0),
        (p::LFO_RATE, 3.5),
        (p::SUB, 3.0),
        (p::VCF_FREQ, 4.5),
        (p::VCF_RES, 2.5),
        (p::VCF_ENV, 2.5),
        (p::VCF_KYBD, 6.0),
        (p::ENV_A, 2.0),
        (p::ENV_D, 6.0),
        (p::ENV_S, 5.0),
        (p::ENV_R, 5.0),
        (p::CHORUS, 2.0),
    ] {
        j.set_param(id, v);
    }

    let mut notes = chord(0.2, 2.4, &[48, 55, 60, 64, 67, 72]);
    notes.extend(chord(2.6, 4.8, &[45, 52, 57, 60, 64, 69]));
    notes.extend(chord(5.0, 7.2, &[41, 48, 53, 57, 60, 65]));
    let seconds = 9.5;

    let transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        ..Default::default()
    };
    let mut h = Harness::new(transport.sample_rate);
    h.transport = transport;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let blocks = (seconds * SR / BLOCK as f64) as usize;
    let begin = Instant::now();
    for b in 0..blocks {
        let (t0, t1) = (
            b as f64 * BLOCK as f64 / SR,
            (b + 1) as f64 * BLOCK as f64 / SR,
        );
        let mut events: Vec<MidiEvent> = Vec::new();
        for n in &notes {
            let at = |t: f64| ((t - t0) * SR) as u32;
            if n.start >= t0 && n.start < t1 {
                events.push(MidiEvent::new(
                    at(n.start),
                    MidiMessage::NoteOn {
                        channel: 0,
                        key: n.key,
                        velocity: 1.0,
                        note_id: 0,
                        tuning: 0.0,
                    },
                ));
            }
            if n.end >= t0 && n.end < t1 {
                events.push(MidiEvent::new(
                    at(n.end),
                    MidiMessage::NoteOff {
                        channel: 0,
                        key: n.key,
                        velocity: 0.0,
                        note_id: 0,
                    },
                ));
            }
        }
        events.sort_by_key(|e| e.offset);
        let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        h.process(&mut j, &mut l, &mut r, &events, &[]);
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
    }
    let cost = begin.elapsed().as_secs_f64() / seconds;
    let peak = left
        .iter()
        .chain(&right)
        .fold(0.0f32, |a, x| a.max(x.abs()));
    rlive_devices::write_wav(path.as_ref(), &left, &right, SR as u32).expect("write WAV");
    println!(
        "wrote {path}: peak {peak:.3}, {:.1} % of one core",
        cost * 100.0
    );
}
