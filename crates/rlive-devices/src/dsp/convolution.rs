//! Uniformly partitioned convolution.

use super::fft::{Complex32, Fft};
use super::util::sanitize;
use std::sync::Arc;

fn partition_size(partition: usize) -> usize {
    partition.next_power_of_two().max(16)
}

/// An impulse response analysed for a [`Convolver`], for sharing between channels and voices
/// and for making away from the audio thread.
#[derive(Debug)]
pub struct ConvolverIr {
    partition: usize,
    /// One spectrum of `partition + 1` bins per partition, in order.
    spectra: Vec<Complex32>,
    partitions: usize,
}

impl ConvolverIr {
    /// Splits `ir` into blocks of `partition` samples, rounded up to a power of two and at
    /// least 16. Allocates.
    pub fn new(ir: &[f32], partition: usize) -> Self {
        let partition = partition_size(partition);
        let bins = partition + 1;
        let mut fft = Fft::new(partition * 2);
        let partitions = ir.len().div_ceil(partition).max(1);
        let mut spectra = vec![Complex32::new(0.0, 0.0); partitions * bins];
        let mut time = vec![0.0f32; partition * 2];
        for (part, chunk) in ir.chunks(partition).enumerate() {
            time.fill(0.0);
            time[..chunk.len()].copy_from_slice(chunk);
            let start = part * bins;
            fft.forward(&mut time, &mut spectra[start..start + bins]);
        }
        Self {
            partition,
            spectra,
            partitions,
        }
    }

    pub fn partition(&self) -> usize {
        self.partition
    }

    pub fn partitions(&self) -> usize {
        self.partitions
    }
}

/// Convolves a stream with an impulse response, at a constant latency of one partition and a
/// constant cost per partition.
#[derive(Clone, Debug)]
pub struct Convolver {
    ir: Option<Arc<ConvolverIr>>,
    partition: usize,
    max_partitions: usize,
    bins: usize,
    fft: Fft,
    /// Spectra of the last `max_partitions` input blocks, as a ring.
    history: Vec<Complex32>,
    history_pos: usize,
    input: Vec<f32>,
    /// The previous block's input and this one's, for the transform.
    overlap: Vec<f32>,
    output: Vec<f32>,
    fill: usize,
    frame: Vec<f32>,
    block_spectrum: Vec<Complex32>,
    accum: Vec<Complex32>,
}

impl Convolver {
    /// Room for an impulse response of up to `max_partitions` partitions of `partition` samples,
    /// which is rounded the same way as in [`ConvolverIr::new`].
    pub fn new(partition: usize, max_partitions: usize) -> Self {
        let partition = partition_size(partition);
        let max_partitions = max_partitions.max(1);
        let bins = partition + 1;
        let fft = Fft::new(partition * 2);
        Self {
            ir: None,
            partition,
            max_partitions,
            bins,
            fft,
            history: vec![Complex32::new(0.0, 0.0); max_partitions * bins],
            history_pos: 0,
            input: vec![0.0; partition],
            overlap: vec![0.0; partition],
            output: vec![0.0; partition],
            fill: 0,
            frame: vec![0.0; partition * 2],
            block_spectrum: vec![Complex32::new(0.0, 0.0); bins],
            accum: vec![Complex32::new(0.0, 0.0); bins],
        }
    }

    /// Ready to play `ir` straight away.
    pub fn with_ir(ir: Arc<ConvolverIr>) -> Self {
        let mut c = Self::new(ir.partition, ir.partitions);
        c.ir = Some(ir);
        c
    }

    pub fn partition(&self) -> usize {
        self.partition
    }

    /// The delay of the convolution, in samples.
    pub fn latency(&self) -> u32 {
        self.partition as u32
    }

    /// Clears everything still ringing.
    pub fn reset(&mut self) {
        self.history.fill(Complex32::new(0.0, 0.0));
        self.history_pos = 0;
        self.input.fill(0.0);
        self.overlap.fill(0.0);
        self.output.fill(0.0);
        self.fill = 0;
    }

    /// Swaps the impulse response without allocating, clearing what was ringing.
    ///
    /// Follows the contract of [`Processor::set_resource`](rlive_core::Processor::set_resource):
    /// returns the replaced response, or `ir` itself if it's the wrong shape for this
    /// convolver, for the engine to retire. It never frees one here.
    pub fn set_ir(&mut self, ir: Arc<ConvolverIr>) -> Option<Arc<ConvolverIr>> {
        if ir.partition != self.partition || ir.partitions > self.max_partitions {
            return Some(ir);
        }
        self.reset();
        self.ir.replace(ir)
    }

    /// Writes one output sample per input sample.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        for (x, y) in input.iter().zip(output.iter_mut()) {
            *y = self.output[self.fill];
            self.input[self.fill] = sanitize(*x);
            self.fill += 1;
            if self.fill == self.partition {
                self.fill = 0;
                self.run_block();
            }
        }
    }

    fn run_block(&mut self) {
        let n = self.partition;
        self.frame[..n].copy_from_slice(&self.overlap);
        self.frame[n..].copy_from_slice(&self.input);
        self.overlap.copy_from_slice(&self.input);
        self.fft.forward(&mut self.frame, &mut self.block_spectrum);

        let bins = self.bins;
        let slot = self.history_pos * bins;
        self.history[slot..slot + bins].copy_from_slice(&self.block_spectrum);

        let Some(ir) = &self.ir else {
            self.output.fill(0.0);
            self.history_pos = (self.history_pos + 1) % self.max_partitions;
            return;
        };
        self.accum.fill(Complex32::new(0.0, 0.0));
        let partitions = ir.partitions.min(self.max_partitions);
        for part in 0..partitions {
            let age = (self.history_pos + self.max_partitions - part) % self.max_partitions;
            let spectrum = &self.history[age * bins..(age + 1) * bins];
            let filter = &ir.spectra[part * bins..(part + 1) * bins];
            for ((a, x), h) in self.accum.iter_mut().zip(spectrum).zip(filter) {
                *a += x * h;
            }
        }
        self.fft.inverse(&mut self.accum, &mut self.frame);
        self.output.copy_from_slice(&self.frame[n..]);
        self.history_pos = (self.history_pos + 1) % self.max_partitions;
    }
}
