//! A FLAC encoder: fixed predictors and Rice-coded residuals, enough to be lossless and small.
//!
//! The stream carries no MD5 of its audio. A zero signature is how the format says "not known".

use std::io::{self, Write};

const BLOCK: usize = 4096;
const MAX_FIXED_ORDER: usize = 4;
const MAX_PARTITION_ORDER: u32 = 6;
const PADDING: usize = 8192;

struct Bits {
    out: Vec<u8>,
    acc: u64,
    used: u32,
}

impl Bits {
    fn new() -> Self {
        Self {
            out: Vec::with_capacity(BLOCK * 4),
            acc: 0,
            used: 0,
        }
    }

    fn put(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 32);
        let masked = if bits == 64 {
            value
        } else {
            value & ((1u64 << bits) - 1)
        };
        self.acc = (self.acc << bits) | masked;
        self.used += bits;
        while self.used >= 8 {
            self.used -= 8;
            self.out.push((self.acc >> self.used) as u8);
        }
    }

    fn put_signed(&mut self, value: i64, bits: u32) {
        self.put(value as u64, bits);
    }

    fn unary_zeros(&mut self, mut zeros: u64) {
        while zeros >= 32 {
            self.put(0, 32);
            zeros -= 32;
        }
        self.put(1, zeros as u32 + 1);
    }

    fn align(&mut self) {
        if self.used > 0 {
            self.put(0, 8 - self.used);
        }
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn zigzag(r: i64) -> u64 {
    ((r << 1) ^ (r >> 63)) as u64
}

/// The residual of a fixed predictor, or `None` when one doesn't fit 32 bits (the format's limit).
fn residual(samples: &[i32], order: usize, out: &mut Vec<i64>) -> bool {
    out.clear();
    for i in order..samples.len() {
        let s = |back: usize| samples[i - back] as i64;
        let r = match order {
            0 => s(0),
            1 => s(0) - s(1),
            2 => s(0) - 2 * s(1) + s(2),
            3 => s(0) - 3 * s(1) + 3 * s(2) - s(3),
            _ => s(0) - 4 * s(1) + 6 * s(2) - 4 * s(3) + s(4),
        };
        if r < i32::MIN as i64 || r > i32::MAX as i64 {
            return false;
        }
        out.push(r);
    }
    true
}

/// The cheapest Rice parameter of a partition and what it costs in bits.
fn best_parameter(residuals: &[i64], max_param: u32) -> (u32, u64) {
    let sum: u64 = residuals.iter().map(|&r| zigzag(r)).sum();
    let n = residuals.len().max(1) as u64;
    let mean = sum / n;
    let guess = 64 - mean.leading_zeros();
    let (low, high) = (guess.saturating_sub(2), (guess + 1).min(max_param));
    let cost = |k: u32| -> u64 {
        residuals
            .iter()
            .map(|&r| (zigzag(r) >> k) + 1 + k as u64)
            .sum()
    };
    let mut best = (low.min(max_param), cost(low.min(max_param)));
    for k in low..=high {
        let c = cost(k);
        if c < best.1 {
            best = (k, c);
        }
    }
    best
}

struct Layout {
    partition_order: u32,
    params: Vec<u32>,
    bits: u64,
}

/// How to divide a block's residual into partitions, and each partition's parameter.
fn plan_partitions(residuals: &[i64], order: usize, block: usize, escape5: bool) -> Layout {
    let max_param = if escape5 { 30 } else { 14 };
    let param_bits = if escape5 { 5 } else { 4 };
    let mut best: Option<Layout> = None;
    for p in 0..=MAX_PARTITION_ORDER {
        let parts = 1usize << p;
        if !block.is_multiple_of(parts) || block >> p <= order {
            break;
        }
        let mut at = 0;
        let mut params = Vec::with_capacity(parts);
        let mut bits = 4u64;
        for part in 0..parts {
            let n = (block >> p) - if part == 0 { order } else { 0 };
            let (k, cost) = best_parameter(&residuals[at..at + n], max_param);
            at += n;
            params.push(k);
            bits += param_bits + cost;
        }
        if best.as_ref().is_none_or(|b| bits < b.bits) {
            best = Some(Layout {
                partition_order: p,
                params,
                bits,
            });
        }
    }
    best.unwrap_or(Layout {
        partition_order: 0,
        params: vec![0],
        bits: u64::MAX,
    })
}

/// Writes a subframe the cheapest way there is, down to writing the samples out plain.
fn write_subframe(b: &mut Bits, samples: &[i32], depth: u32) {
    let mut best: Option<(usize, Layout, Vec<i64>)> = None;
    let mut scratch = Vec::with_capacity(samples.len());
    let escape5 = depth > 16;
    for order in 0..=MAX_FIXED_ORDER.min(samples.len().saturating_sub(1)) {
        if !residual(samples, order, &mut scratch) {
            continue;
        }
        let layout = plan_partitions(&scratch, order, samples.len(), escape5);
        if best.as_ref().is_none_or(|(_, l, _)| layout.bits < l.bits) {
            best = Some((order, layout, scratch.clone()));
        }
    }
    let verbatim = samples.len() as u64 * depth as u64;
    let Some((order, layout, res)) = best
        .filter(|(o, l, _)| l.bits != u64::MAX && l.bits + (*o as u64 * depth as u64) < verbatim)
    else {
        b.put(0b0000_0010, 8);
        samples.iter().for_each(|&s| b.put_signed(s as i64, depth));
        return;
    };
    b.put(0, 1);
    b.put(0b001000 | order as u64, 6);
    b.put(0, 1);
    for &s in &samples[..order] {
        b.put_signed(s as i64, depth);
    }
    let param_bits = if escape5 { 5 } else { 4 };
    b.put(u64::from(escape5), 2);
    b.put(layout.partition_order as u64, 4);
    let mut at = 0;
    for (part, &k) in layout.params.iter().enumerate() {
        let n = (samples.len() >> layout.partition_order) - if part == 0 { order } else { 0 };
        b.put(k as u64, param_bits);
        for &r in &res[at..at + n] {
            let u = zigzag(r);
            b.unary_zeros(u >> k);
            if k > 0 {
                b.put(u, k);
            }
        }
        at += n;
    }
}

fn utf8_number(out: &mut Vec<u8>, n: u64) {
    if n < 0x80 {
        out.push(n as u8);
        return;
    }
    let extra = match n {
        n if n < 0x800 => 1,
        n if n < 0x1_0000 => 2,
        n if n < 0x20_0000 => 3,
        n if n < 0x400_0000 => 4,
        n if n < 0x8000_0000 => 5,
        _ => 6,
    };
    let first = (0xffu16 << (7 - extra)) as u8;
    out.push(first | (n >> (extra * 6)) as u8);
    for i in (0..extra).rev() {
        out.push(0x80 | ((n >> (i * 6)) & 0x3f) as u8);
    }
}

fn depth_code(depth: u32) -> u64 {
    match depth {
        8 => 0b001,
        12 => 0b010,
        16 => 0b100,
        20 => 0b101,
        24 => 0b110,
        _ => 0b111,
    }
}

fn write_frame(
    out: &mut impl Write,
    channels: &[&[i32]],
    number: u64,
    depth: u32,
) -> io::Result<()> {
    let block = channels[0].len();
    let mut head = vec![0xff, 0xf8];
    // A whole block has a code of its own; a shorter last one gives its size in 16 bits.
    let size_code: u8 = if block == BLOCK { 0b1100 } else { 0b0111 };
    head.push(size_code << 4);
    head.push(((channels.len() as u8 - 1) << 4) | ((depth_code(depth) as u8) << 1));
    utf8_number(&mut head, number);
    if size_code == 0b0111 {
        head.extend_from_slice(&((block - 1) as u16).to_be_bytes());
    }
    let crc = crc8(&head);
    head.push(crc);
    let mut b = Bits::new();
    b.out.extend_from_slice(&head);
    for samples in channels {
        write_subframe(&mut b, samples, depth);
    }
    b.align();
    let crc = crc16(&b.out);
    b.out.extend_from_slice(&crc.to_be_bytes());
    out.write_all(&b.out)
}

/// Writes whole samples, one slice per channel, as a FLAC stream.
pub fn encode(
    out: &mut impl Write,
    channels: &[&[i32]],
    sample_rate: u32,
    depth: u32,
) -> io::Result<()> {
    let frames = channels.first().map_or(0, |c| c.len());
    if channels.is_empty() || channels.len() > 8 || channels.iter().any(|c| c.len() != frames) {
        return Err(io::Error::other("channels of unequal length"));
    }
    out.write_all(b"fLaC")?;
    // Readers probe for a second frame, so even a very short stream is cut in two when it can be.
    let block = if frames > BLOCK {
        BLOCK
    } else {
        frames.div_ceil(2).max(16).min(frames.max(1))
    };
    let mut info = Bits::new();
    info.put(0, 8); // STREAMINFO, with another metadata block after it.
    info.put(34, 24);
    info.put(block.max(16) as u64, 16);
    info.put(block.max(16) as u64, 16);
    info.put(0, 24);
    info.put(0, 24);
    info.put(sample_rate as u64, 20);
    info.put(channels.len() as u64 - 1, 3);
    info.put(depth as u64 - 1, 5);
    info.put((frames as u64 >> 32) & 0xf, 4);
    info.put(frames as u64 & 0xffff_ffff, 32);
    info.put(0, 32);
    info.put(0, 32);
    info.put(0, 32);
    info.put(0, 32);
    out.write_all(&info.out)?;
    // Padding, as the reference encoder leaves, so a tag can be added without rewriting the file.
    out.write_all(&[
        0x81,
        (PADDING >> 16) as u8,
        (PADDING >> 8) as u8,
        PADDING as u8,
    ])?;
    out.write_all(&vec![0u8; PADDING])?;
    for (number, at) in (0..frames).step_by(block.max(1)).enumerate() {
        let end = (at + block).min(frames);
        let frame: Vec<&[i32]> = channels.iter().map(|c| &c[at..end]).collect();
        write_frame(out, &frame, number as u64, depth)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rlive-flac-{}-{name}.flac", std::process::id()))
    }

    fn signal(n: usize, amp: f64) -> (Vec<i32>, Vec<i32>) {
        let mut noise = 12345u32;
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            noise = noise.wrapping_mul(1664525).wrapping_add(1013904223);
            let hiss = (noise >> 16) as f64 / 65536.0 - 0.5;
            left.push((((i as f64 * 0.01).sin() * 0.7 + hiss * 0.1) * amp) as i32);
            right.push((((i as f64 * 0.003).cos() * 0.5) * amp) as i32);
        }
        (left, right)
    }

    fn decodes_exactly(depth: u32, n: usize, name: &str) {
        let amp = if depth == 32 {
            i32::MAX as f64
        } else {
            ((1u64 << (depth - 1)) - 1) as f64
        };
        let (left, right) = signal(n, amp);
        let p = path(name);
        let mut file = std::fs::File::create(&p).unwrap();
        encode(&mut file, &[&left, &right], 44_100, depth).unwrap();
        drop(file);
        let decoded = crate::decode(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(decoded.sample_rate, 44_100, "{name}");
        assert_eq!(decoded.frames(), n, "{name}");
        let tolerance = 1.5 / amp as f32;
        for i in (0..n).step_by(97) {
            assert!(
                (decoded.left[i] - (left[i] as f64 / amp) as f32).abs() <= tolerance.max(2e-7),
                "{name} left[{i}]"
            );
            assert!(
                (decoded.right[i] - (right[i] as f64 / amp) as f32).abs() <= tolerance.max(2e-7),
                "{name} right[{i}]"
            );
        }
    }

    #[test]
    fn every_depth_decodes_back_exactly() {
        decodes_exactly(16, 10_000, "d16");
        decodes_exactly(24, 10_000, "d24");
        decodes_exactly(32, 10_000, "d32");
    }

    #[test]
    fn blocks_of_any_length_decode() {
        decodes_exactly(16, 17, "seventeen");
        decodes_exactly(16, 40, "forty");
        decodes_exactly(16, 200, "two-hundred");
        decodes_exactly(16, 4096, "block");
        decodes_exactly(16, 4097, "block-and-one");
        decodes_exactly(24, 123, "short");
    }

    #[test]
    fn it_compresses() {
        let (left, right) = signal(44_100, 32_767.0);
        let mut out = Vec::new();
        encode(&mut out, &[&left, &right], 44_100, 16).unwrap();
        assert!(out.len() < 44_100 * 4 * 3 / 4, "{} bytes", out.len());
    }

    #[test]
    fn crcs_match_the_format() {
        // From the reference check values for these polynomials.
        assert_eq!(crc8(b"123456789"), 0xf4);
        assert_eq!(crc16(b"123456789"), 0xfee8);
    }
}
