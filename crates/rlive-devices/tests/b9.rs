//! Batch B9: the External devices on their own: gains, mixing and latency. The route to the
//! hardware itself is tested against the engine, in the app's session tests.

use rlive_core::{
    AudioBuffer, AudioInput, AudioOutput, EventQueue, ParamOutQueue, ProcessContext, Processor,
    TransportInfo, ViewWriter,
};
use rlive_devices::devices::b9::{external_audio_effect as eae, external_instrument as ei};
use rlive_devices::{find, DeviceInit};
use rlive_testkit::assert_no_alloc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 480;

fn device(id: &str) -> Box<dyn Processor> {
    let info = find(id).unwrap();
    let mut p = (info.create)(&DeviceInit::default());
    p.activate(SR, BLOCK);
    p
}

/// One block: what comes back, what went out and the main output.
fn block(
    p: &mut dyn Processor,
    input: &[f32],
    ret: Option<&[f32]>,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (input.to_vec(), input.to_vec());
    let (mut al, mut ar) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let transport = TransportInfo::default();
    let mut q = EventQueue::with_capacity(4);
    let mut param = ParamOutQueue::with_capacity(4);
    let view = ViewWriter::none();
    {
        let mut outs = [AudioOutput {
            left: &mut al,
            right: &mut ar,
        }];
        assert_no_alloc(|| {
            p.process(&mut ProcessContext {
                audio: AudioBuffer {
                    left: &mut l,
                    right: &mut r,
                },
                sidechain: ret.map(|f| AudioInput { left: f, right: f }),
                aux_out: &mut outs,
                events: &[],
                events_out: &mut q,
                params: &[],
                transport: &transport,
                param_out: &mut param,
                mod_out: &mut [],
                view: &view,
            })
        });
    }
    (l, al, ar)
}

const SIG: [f32; 8] = [0.5, -0.25, 0.75, 0.0, 0.0, 0.125, -0.5, 0.3];

fn input() -> Vec<f32> {
    let mut v = vec![0.0; BLOCK];
    v[..8].copy_from_slice(&SIG);
    v
}

#[test]
fn it_sends_the_signal_with_gain_and_mixes_the_return() {
    let mut p = device("rlive.external_audio_effect");
    p.set_param(eae::GAIN_OUT, -6.020599);
    let ret = vec![0.25f32; BLOCK];
    let (out, sent, _) = block(&mut *p, &input(), Some(&ret));
    assert!((sent[0] - 0.25).abs() < 1e-4, "gain out");
    assert_eq!(out[0], 0.25, "100% wet");
    p.set_param(eae::DRY_WET, 50.0);
    let (out, _, _) = block(&mut *p, &input(), Some(&ret));
    assert!((out[0] - (0.5 + 0.25) * 0.5).abs() < 1e-4);
}

#[test]
fn invert_flips_what_comes_back() {
    let mut p = device("rlive.external_audio_effect");
    p.set_param(eae::INVERT, 1.0);
    p.set_param(eae::GAIN_IN, -6.020599);
    let ret = vec![0.25f32; BLOCK];
    let (out, _, _) = block(&mut *p, &input(), Some(&ret));
    assert!((out[0] + 0.125).abs() < 1e-4, "{}", out[0]);
}

#[test]
fn nothing_routed_back_leaves_the_track_intact() {
    let mut p = device("rlive.external_audio_effect");
    let (out, _, _) = block(&mut *p, &input(), None);
    assert_eq!(&out[..8], &SIG);
}

#[test]
fn hardware_latency_is_in_ms_or_samples_and_delays_the_dry_path() {
    let mut p = device("rlive.external_audio_effect");
    p.set_param(eae::DRY_WET, 0.0);
    assert_eq!(p.latency(), 0);
    p.set_param(eae::LATENCY_MS, 1.0);
    assert_eq!(p.latency(), 48, "1 ms at 48 kHz");
    p.set_param(eae::LATENCY_MODE, 1.0);
    p.set_param(eae::LATENCY_SAMPLES, 100.0);
    assert_eq!(p.latency(), 100, "samples stay samples at any rate");
    let (out, _, _) = block(&mut *p, &input(), Some(&vec![0.0f32; BLOCK]));
    assert_eq!(out[99], 0.0);
    assert_eq!(out[100], 0.5, "the dry path meets the wet one");
    assert_eq!(out[102], 0.75);
}

#[test]
fn the_instrument_adds_what_comes_back_with_its_gain() {
    let mut p = device("rlive.external_instrument");
    p.set_param(ei::GAIN, -6.020599);
    let ret = vec![0.5f32; BLOCK];
    let (out, _, _) = block(&mut *p, &vec![0.0; BLOCK], Some(&ret));
    assert!((out[10] - 0.25).abs() < 1e-3, "{}", out[10]);
    let (out, _, _) = block(&mut *p, &input(), None);
    assert_eq!(out[0], 0.5, "what was there stays");
    p.set_param(ei::LATENCY_MS, 2.0);
    assert_eq!(p.latency(), 96);
    p.set_param(ei::INTERNAL_LATENCY, 512.0);
    assert_eq!(p.latency(), 608, "the router's path adds to it");
}
