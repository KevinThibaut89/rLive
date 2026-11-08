use crate::dsp::convolution::*;
use crate::dsp::util::Rng;
use std::sync::Arc;

fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..n).map(|_| rng.bipolar()).collect()
}

fn direct_convolution(x: &[f32], h: &[f32]) -> Vec<f32> {
    let mut y = vec![0.0f64; x.len() + h.len() - 1];
    for (i, a) in x.iter().enumerate() {
        for (j, b) in h.iter().enumerate() {
            y[i + j] += (*a as f64) * (*b as f64);
        }
    }
    y.into_iter().map(|v| v as f32).collect()
}

fn run(c: &mut Convolver, input: &[f32], chunk: usize) -> Vec<f32> {
    let mut output = vec![0.0; input.len()];
    for (i, o) in input.chunks(chunk).zip(output.chunks_mut(chunk)) {
        c.process(i, o);
    }
    output
}

#[test]
fn it_equals_direct_convolution() {
    let input = noise(6_000, 2);
    for ir_len in [1usize, 37, 256, 257, 1_000] {
        let ir = noise(ir_len, 3);
        let want = direct_convolution(&input, &ir);
        for partition in [64usize, 128] {
            for chunk in [1, 50, 256] {
                let mut c = Convolver::with_ir(Arc::new(ConvolverIr::new(&ir, partition)));
                let got = run(&mut c, &input, chunk);
                let latency = c.latency() as usize;
                let mut worst = 0.0f32;
                for t in 0..input.len() - latency {
                    worst = worst.max((got[t + latency] - want[t]).abs());
                }
                assert!(
                    worst < 2e-3,
                    "ir {ir_len} partition {partition} chunk {chunk}: {worst}"
                );
            }
        }
    }
}

#[test]
fn the_latency_is_one_partition() {
    let mut c = Convolver::with_ir(Arc::new(ConvolverIr::new(&[1.0], 128)));
    let mut input = vec![0.0; 1_024];
    input[0] = 1.0;
    let out = run(&mut c, &input, 64);
    assert_eq!(rlive_testkit::onset(&out, 0.5), Some(128));
    assert!((out[128] - 1.0).abs() < 1e-4);
}

#[test]
fn an_empty_convolver_is_silent() {
    let mut c = Convolver::new(128, 4);
    let out = run(&mut c, &noise(1_024, 1), 256);
    assert!(out.iter().all(|x| *x == 0.0));
}

#[test]
fn a_new_response_replaces_the_old_one() {
    let mut c = Convolver::new(64, 8);
    let first = Arc::new(ConvolverIr::new(&[1.0], 64));
    let second = Arc::new(ConvolverIr::new(&[0.5], 64));
    assert!(c.set_ir(first.clone()).is_none());
    let replaced = c.set_ir(second).expect("the old response");
    assert!(Arc::ptr_eq(&replaced, &first));
}

#[test]
fn a_response_of_the_wrong_shape_is_refused() {
    let mut c = Convolver::new(64, 2);
    let too_long = Arc::new(ConvolverIr::new(&vec![0.5; 1_000], 64));
    let refused = c.set_ir(too_long.clone()).expect("refused");
    assert!(Arc::ptr_eq(&refused, &too_long));
    let wrong_size = Arc::new(ConvolverIr::new(&[1.0], 128));
    assert!(c.set_ir(wrong_size).is_some());
}

#[test]
fn a_partition_size_is_rounded_the_same_everywhere() {
    let mut c = Convolver::with_ir(Arc::new(ConvolverIr::new(&[1.0], 100)));
    assert_eq!(c.partition(), 128);
    let mut input = vec![0.0; 512];
    input[0] = 1.0;
    let out = run(&mut c, &input, 512);
    assert_eq!(rlive_testkit::onset(&out, 0.5), Some(128));
}

#[test]
fn reset_clears_the_tail() {
    let mut c = Convolver::with_ir(Arc::new(ConvolverIr::new(&noise(1_000, 3), 64)));
    run(&mut c, &noise(512, 1), 64);
    c.reset();
    let out = run(&mut c, &vec![0.0; 1_024], 64);
    assert!(out.iter().all(|x| x.abs() < 1e-6));
}
