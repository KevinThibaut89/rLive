//! What happens to rendered audio on its way into a file: gain, sample rate, dither and encoding.

use crate::flac;
use crate::writer::{AudioWriter, BitDepth, FileType};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::io;
use std::path::{Path, PathBuf};

/// The file formats Export can write losslessly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExportType {
    #[default]
    Wav,
    Aiff,
    Flac,
}

impl ExportType {
    pub const ALL: [ExportType; 3] = [ExportType::Wav, ExportType::Aiff, ExportType::Flac];

    pub fn label(self) -> &'static str {
        match self {
            ExportType::Wav => "WAV",
            ExportType::Aiff => "AIFF",
            ExportType::Flac => "FLAC",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportType::Wav => "wav",
            ExportType::Aiff => "aif",
            ExportType::Flac => "flac",
        }
    }
}

/// The noise added before samples are rounded to a lower bit depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Dither {
    None,
    Rectangular,
    #[default]
    Triangular,
}

impl Dither {
    pub const ALL: [Dither; 3] = [Dither::None, Dither::Rectangular, Dither::Triangular];

    pub fn label(self) -> &'static str {
        match self {
            Dither::None => "No Dither",
            Dither::Rectangular => "Rectangular",
            Dither::Triangular => "Triangular",
        }
    }
}

/// The biggest whole sample of a depth, which full scale maps to.
fn full_scale(depth: BitDepth) -> f64 {
    match depth {
        BitDepth::Bits16 => i16::MAX as f64,
        BitDepth::Bits24 => 8_388_607.0,
        BitDepth::Bits32 => i32::MAX as f64,
    }
}

/// A plain generator: the noise only has to be white, and the same each time.
struct Noise(u64);

impl Noise {
    /// Uniform in -0.5..0.5.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    }
}

const NOISE_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// Adds dither for a reduction to `depth`. A 32-bit file loses nothing to speak of, so it's left
/// exactly as it came.
pub fn apply_dither(audio: &mut [f32], dither: Dither, depth: BitDepth) {
    if depth == BitDepth::Bits32 || dither == Dither::None {
        return;
    }
    let lsb = 1.0 / full_scale(depth);
    let mut noise = Noise(NOISE_SEED);
    for s in audio {
        let amount = match dither {
            Dither::None => 0.0,
            Dither::Rectangular => noise.next(),
            Dither::Triangular => noise.next() + noise.next(),
        };
        *s = (*s as f64 + amount * lsb) as f32;
    }
}

/// Brings the highest peak to full scale. Silence stays silent.
pub fn normalize(channels: &mut [&mut [f32]]) {
    let peak = channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |p, s| p.max(s.abs()));
    if peak <= 1e-9 {
        return;
    }
    let gain = 1.0 / peak;
    for c in channels {
        c.iter_mut().for_each(|s| *s = (*s * gain).clamp(-1.0, 1.0));
    }
}

const SINC_ZEROS: usize = 32;
const TABLE_RES: usize = 512;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..64 {
        term *= (x / 2.0 / k as f64).powi(2);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// A Kaiser-windowed sinc from 0 to [`SINC_ZEROS`] zero crossings.
fn sinc_table() -> Vec<f64> {
    let beta = 12.0;
    let scale = bessel_i0(beta);
    (0..=SINC_ZEROS * TABLE_RES + 1)
        .map(|i| {
            let u = i as f64 / TABLE_RES as f64;
            let sinc = if i == 0 {
                1.0
            } else {
                (PI * u).sin() / (PI * u)
            };
            let edge = (u / SINC_ZEROS as f64).min(1.0);
            sinc * bessel_i0(beta * (1.0 - edge * edge).sqrt()) / scale
        })
        .collect()
}

/// Changes a signal's sample rate with a band-limited filter. The length follows the ratio.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let table = sinc_table();
    let ratio = to as f64 / from as f64;
    // Just under the new Nyquist, so the top of the transition band folds into nothing audible.
    let cut = ratio.min(1.0) * 0.97;
    let reach = SINC_ZEROS as f64 / cut;
    let frames = (input.len() as f64 * ratio).round() as usize;
    let last = input.len() as i64 - 1;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let pos = i as f64 / ratio;
        let first = (pos - reach).ceil().max(0.0) as i64;
        let end = ((pos + reach).floor() as i64).min(last);
        let mut sum = 0.0;
        for j in first..=end {
            let u = (pos - j as f64).abs() * cut * TABLE_RES as f64;
            let at = u as usize;
            let frac = u - at as f64;
            let w = if at + 1 < table.len() {
                table[at] + (table[at + 1] - table[at]) * frac
            } else {
                0.0
            };
            sum += input[j as usize] as f64 * w;
        }
        out.push((sum * cut) as f32);
    }
    out
}

/// What an uncompressed-quality file is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub kind: ExportType,
    pub depth: BitDepth,
    pub dither: Dither,
}

/// Next to `path`, for the file being written.
fn temp_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

fn round_sample(sample: f32, depth: BitDepth) -> i32 {
    let s = sample.clamp(-1.0, 1.0) as f64;
    (s * full_scale(depth)).round() as i32
}

/// Writes one or two channels as PCM, replacing `path` only once the new file is whole.
pub fn write_pcm(
    path: &Path,
    channels: &[&[f32]],
    sample_rate: u32,
    format: PcmFormat,
) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = temp_of(path);
    let result = write_pcm_to(&tmp, channels, sample_rate, format);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    std::fs::rename(&tmp, path)
}

fn write_pcm_to(
    path: &Path,
    channels: &[&[f32]],
    sample_rate: u32,
    format: PcmFormat,
) -> io::Result<()> {
    let n = channels.first().map_or(0, |c| c.len());
    if channels.iter().any(|c| c.len() != n) {
        return Err(io::Error::other("channels of unequal length"));
    }
    let mut dithered: Vec<Vec<f32>> = channels.iter().map(|c| c.to_vec()).collect();
    for c in &mut dithered {
        apply_dither(c, format.dither, format.depth);
    }
    match format.kind {
        ExportType::Flac => {
            let whole: Vec<Vec<i32>> = dithered
                .iter()
                .map(|c| c.iter().map(|&s| round_sample(s, format.depth)).collect())
                .collect();
            let refs: Vec<&[i32]> = whole.iter().map(Vec::as_slice).collect();
            let mut out = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
            flac::encode(&mut out, &refs, sample_rate, format.depth.bits() as u32)?;
            io::Write::flush(&mut out)?;
            out.get_ref().sync_all()
        }
        ExportType::Wav | ExportType::Aiff => {
            let kind = if format.kind == ExportType::Wav {
                FileType::Wav
            } else {
                FileType::Aiff
            };
            let mut w =
                AudioWriter::create(path, kind, format.depth, channels.len() as u16, sample_rate)?;
            let mut frame = Vec::with_capacity(8192);
            for at in (0..n).step_by(4096) {
                let end = (at + 4096).min(n);
                frame.clear();
                for i in at..end {
                    frame.extend(dithered.iter().map(|c| c[i]));
                }
                w.write(&frame)?;
            }
            w.finish().map(|_| ())
        }
    }
}

/// Whether this build can write MP3 files.
pub const MP3_AVAILABLE: bool = cfg!(feature = "mp3");
/// The sample rates an MP3 can be.
pub const MP3_RATES: [u32; 3] = [32_000, 44_100, 48_000];

/// The MP3 rate nearest above or at `rate`, or the highest there is.
pub fn mp3_rate_for(rate: u32) -> u32 {
    MP3_RATES.into_iter().find(|r| *r >= rate).unwrap_or(48_000)
}

/// Writes a 320 kbps constant-bit-rate MP3, resampling first if the rate isn't one MP3 has.
#[cfg(feature = "mp3")]
pub fn write_mp3(path: &Path, left: &[f32], right: &[f32], sample_rate: u32) -> io::Result<()> {
    use mp3lame_encoder::{Bitrate, Builder, DualPcm, FlushNoGap, Quality};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let target = mp3_rate_for(sample_rate);
    let (left, right) = if target == sample_rate {
        (left.to_vec(), right.to_vec())
    } else {
        (
            resample(left, sample_rate, target),
            resample(right, sample_rate, target),
        )
    };
    let fail = |e: String| io::Error::other(format!("MP3 encoder: {e}"));
    let mut builder = Builder::new().ok_or_else(|| fail("could not start".into()))?;
    builder
        .set_num_channels(2)
        .map_err(|e| fail(e.to_string()))?;
    builder
        .set_sample_rate(target)
        .map_err(|e| fail(e.to_string()))?;
    builder
        .set_brate(Bitrate::Kbps320)
        .map_err(|e| fail(e.to_string()))?;
    builder
        .set_quality(Quality::Best)
        .map_err(|e| fail(e.to_string()))?;
    let mut encoder = builder.build().map_err(|e| fail(e.to_string()))?;
    let widen = |c: &[f32]| -> Vec<i16> {
        c.iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
            .collect()
    };
    let (l, r) = (widen(&left), widen(&right));
    let mut out = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(l.len()));
    encoder
        .encode_to_vec(
            DualPcm {
                left: &l,
                right: &r,
            },
            &mut out,
        )
        .map_err(|e| fail(e.to_string()))?;
    encoder
        .flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| fail(e.to_string()))?;
    let tmp = temp_of(path);
    std::fs::write(&tmp, &out)?;
    std::fs::rename(&tmp, path)
}

/// Writes a 320 kbps constant-bit-rate MP3. This build has no encoder.
#[cfg(not(feature = "mp3"))]
pub fn write_mp3(_: &Path, _: &[f32], _: &[f32], _: u32) -> io::Result<()> {
    Err(io::Error::other("this build has no MP3 encoder"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    fn sine(hz: f64, rate: f64, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f64 * hz * 2.0 * PI / rate).sin() as f32 * amp)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |p, s| p.max(s.abs()))
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rlive-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn resampling_keeps_a_tone_and_its_length() {
        let input = sine(1000.0, 48_000.0, 48_000, 0.5);
        let out = resample(&input, 48_000, 44_100);
        assert_eq!(out.len(), 44_100);
        let body = &out[2000..42_000];
        assert!((peak(body) - 0.5).abs() < 0.01, "{}", peak(body));
        let want = sine(1000.0, 44_100.0, 44_100, 0.5);
        let err = body
            .iter()
            .zip(&want[2000..42_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.005, "{err}");
    }

    #[test]
    fn what_is_above_the_new_nyquist_does_not_fold_back() {
        let input = sine(15_000.0, 48_000.0, 48_000, 0.5);
        let out = resample(&input, 48_000, 22_050);
        let body = &out[2000..20_000];
        assert!(rms(body) < 1e-3, "{}", rms(body));
    }

    #[test]
    fn upsampling_works_too() {
        let input = sine(440.0, 22_050.0, 22_050, 0.25);
        let out = resample(&input, 22_050, 44_100);
        assert_eq!(out.len(), 44_100);
        assert!((peak(&out[2000..40_000]) - 0.25).abs() < 0.01);
    }

    #[test]
    fn normalize_brings_the_peak_to_full_scale_across_channels() {
        let mut l = vec![0.25f32, -0.5];
        let mut r = vec![0.1f32, 0.0];
        normalize(&mut [&mut l, &mut r]);
        assert_eq!(l, [0.5, -1.0]);
        assert!((r[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn normalize_leaves_silence_alone() {
        let mut l = vec![0.0f32; 8];
        normalize(&mut [&mut l]);
        assert_eq!(l, [0.0; 8]);
    }

    fn deviation(x: &[f32], depth: BitDepth) -> f64 {
        let lsb = 1.0 / full_scale(depth);
        x.iter()
            .map(|&s| (s as f64 / lsb).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn dither_noise_is_as_wide_as_each_kind_says() {
        let n = 100_000;
        let mut rect = vec![0.0f32; n];
        apply_dither(&mut rect, Dither::Rectangular, BitDepth::Bits16);
        let mut tri = vec![0.0f32; n];
        apply_dither(&mut tri, Dither::Triangular, BitDepth::Bits16);
        let (r, t) = (
            deviation(&rect, BitDepth::Bits16),
            deviation(&tri, BitDepth::Bits16),
        );
        assert!(r > 0.45 && r <= 0.5001, "{r}");
        assert!(t > 0.8 && t <= 1.0001, "{t}");
        // Triangular has twice the variance of one rectangular source, and then some.
        assert!(rms(&tri) > 1.3 * rms(&rect), "{} {}", rms(&tri), rms(&rect));
    }

    #[test]
    fn nothing_is_added_when_there_is_no_reduction_or_no_dither() {
        let mut a = vec![0.123f32; 64];
        apply_dither(&mut a, Dither::Triangular, BitDepth::Bits32);
        assert_eq!(a, vec![0.123f32; 64]);
        apply_dither(&mut a, Dither::None, BitDepth::Bits16);
        assert_eq!(a, vec![0.123f32; 64]);
    }

    #[test]
    fn a_quantized_quiet_tone_keeps_its_harmonics_clean_with_dither() {
        // A tone below one bit: undithered it rounds away to nothing.
        let quiet = sine(997.0, 44_100.0, 44_100, 0.4 / i16::MAX as f32);
        let plain = temp("plain.wav");
        let tri = temp("tri.wav");
        let mut format = PcmFormat {
            kind: ExportType::Wav,
            depth: BitDepth::Bits16,
            dither: Dither::None,
        };
        write_pcm(&plain, &[&quiet], 44_100, format).unwrap();
        format.dither = Dither::Triangular;
        write_pcm(&tri, &[&quiet], 44_100, format).unwrap();
        let p = decode(&plain).unwrap();
        let t = decode(&tri).unwrap();
        assert_eq!(peak(&p.left), 0.0, "rounds away");
        assert!(peak(&t.left) > 0.0, "survives in the noise");
        std::fs::remove_file(plain).ok();
        std::fs::remove_file(tri).ok();
    }

    #[test]
    fn every_type_and_depth_writes_a_file_that_reads_back() {
        let tone = sine(440.0, 44_100.0, 5000, 0.5);
        for kind in ExportType::ALL {
            for depth in BitDepth::ALL {
                let p = temp(&format!("all-{kind:?}-{depth:?}.{}", kind.extension()));
                write_pcm(
                    &p,
                    &[&tone, &tone],
                    44_100,
                    PcmFormat {
                        kind,
                        depth,
                        dither: Dither::None,
                    },
                )
                .unwrap();
                let got = decode(&p).unwrap();
                assert_eq!(got.sample_rate, 44_100, "{kind:?} {depth:?}");
                assert_eq!(got.frames(), 5000, "{kind:?} {depth:?}");
                let err = got
                    .left
                    .iter()
                    .zip(&tone)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f32::max);
                assert!(err < 1e-4, "{kind:?} {depth:?}: {err}");
                assert!(!temp_of(&p).exists(), "the part file is gone");
                std::fs::remove_file(p).ok();
            }
        }
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_alone() {
        let p = temp("keep.wav");
        std::fs::write(&p, b"precious").unwrap();
        let (a, b) = (vec![0.0f32; 4], vec![0.0f32; 5]);
        let format = PcmFormat {
            kind: ExportType::Wav,
            depth: BitDepth::Bits24,
            dither: Dither::None,
        };
        assert!(write_pcm(&p, &[&a, &b], 44_100, format).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"precious");
        std::fs::remove_file(p).ok();
    }

    #[cfg(feature = "mp3")]
    #[test]
    fn an_mp3_decodes_to_about_the_same_audio() {
        let tone = sine(440.0, 44_100.0, 44_100, 0.5);
        let p = temp("tone.mp3");
        write_mp3(&p, &tone, &tone, 44_100).unwrap();
        let got = decode(&p).unwrap();
        assert_eq!(got.sample_rate, 44_100);
        let extra = got.frames().abs_diff(44_100);
        assert!(extra < 4000, "{} frames", got.frames());
        assert!(
            (rms(&got.left[8000..30_000]) - rms(&tone[8000..30_000])).abs() < 0.02,
            "{}",
            rms(&got.left[8000..30_000])
        );
        let bytes = std::fs::metadata(&p).unwrap().len();
        assert!((36_000..44_000).contains(&bytes), "320 kbps: {bytes} bytes");
        std::fs::remove_file(p).ok();
    }

    #[cfg(feature = "mp3")]
    #[test]
    fn an_mp3_at_a_rate_it_does_not_have_is_resampled() {
        let tone = sine(440.0, 96_000.0, 96_000, 0.5);
        let p = temp("tone96.mp3");
        write_mp3(&p, &tone, &tone, 96_000).unwrap();
        assert_eq!(decode(&p).unwrap().sample_rate, 48_000);
        std::fs::remove_file(p).ok();
    }
}
