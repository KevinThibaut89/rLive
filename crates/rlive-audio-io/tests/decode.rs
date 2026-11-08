//! Each supported format, decoded and compared against what went in.
//!
//! WAV, AIFF, AIFF-C and FLAC are written here byte by byte, so the expected PCM is exact. The
//! lossy formats have no encoder in this workspace: `tests/fixtures` holds files made once from
//! the same sines these tests describe (0.5 s, 440 Hz left at 0.5, 880 Hz right at 0.25), with
//! libvorbis, LAME and macOS `afconvert`, and they are compared within what the codecs allow.

use rlive_audio_io::decode::decode;
use rlive_audio_io::{probe, DecodeError, DecodedAudio};
use rlive_testkit::measure::tone_amplitude;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const RATE: u32 = 44_100;
const FRAMES: usize = 2_000;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn temp_dir() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rlive-decode-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Reference stereo PCM as integers of `bits` bits, full scale and a little past zero crossings.
fn reference(bits: u32) -> (Vec<i32>, Vec<i32>) {
    let full = (1i64 << (bits - 1)) - 1;
    let wave = |i: usize, hz: f64| {
        ((i as f64 / RATE as f64 * hz * std::f64::consts::TAU).sin() * full as f64) as i32
    };
    (
        (0..FRAMES).map(|i| wave(i, 330.0)).collect(),
        (0..FRAMES).map(|i| wave(i, 990.0)).collect(),
    )
}

fn to_float(samples: &[i32], bits: u32) -> Vec<f32> {
    let scale = (1i64 << (bits - 1)) as f64;
    samples.iter().map(|s| (*s as f64 / scale) as f32).collect()
}

fn assert_same(got: &[f32], want: &[f32], tolerance: f32, what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    for (i, (a, b)) in got.iter().zip(want).enumerate() {
        assert!((a - b).abs() <= tolerance, "{what}: frame {i}: {a} vs {b}");
    }
}

// -------------------------------------------------------------------- WAV

fn write_wav(path: &Path, bits: u16, float: bool, channels: u16, rate: u32) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format: if float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let (l, r) = reference(bits.into());
    for i in 0..FRAMES {
        let picks = if channels == 1 {
            vec![l[i]]
        } else {
            vec![l[i], r[i]]
        };
        for &v in &picks {
            match (float, bits) {
                (true, _) => w
                    .write_sample(v as f32 / 2f32.powi(bits as i32 - 1))
                    .unwrap(),
                (_, 8) => w.write_sample(v as i8).unwrap(),
                (_, 16) => w.write_sample(v as i16).unwrap(),
                _ => w.write_sample(v).unwrap(),
            }
        }
    }
    w.finalize().unwrap();
}

#[test]
fn wav_at_every_bit_depth_matches_the_reference() {
    let dir = temp_dir();
    for (bits, float) in [
        (8, false),
        (16, false),
        (24, false),
        (32, false),
        (32, true),
    ] {
        let path = dir.join(format!("{bits}{float}.wav"));
        write_wav(&path, bits, float, 2, RATE);
        let a = decode(&path).unwrap();
        let (l, r) = reference(bits.into());
        let tolerance = if bits >= 24 { 2e-7 } else { 1e-7 };
        assert_same(&a.left, &to_float(&l, bits.into()), tolerance, "left");
        assert_same(&a.right, &to_float(&r, bits.into()), tolerance, "right");
        assert_eq!(a.sample_rate, RATE);
        assert!(a.is_stereo());
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn mono_files_read_the_same_on_both_channels() {
    let dir = temp_dir();
    let path = dir.join("m.wav");
    write_wav(&path, 16, false, 1, 22_050);
    let a = decode(&path).unwrap();
    assert!(!a.is_stereo());
    assert_eq!(a.left, a.right);
    assert_eq!(a.sample_rate, 22_050);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn other_sample_rates_are_kept_as_they_are() {
    let dir = temp_dir();
    for rate in [8_000, 11_025, 88_200, 96_000, 192_000] {
        let path = dir.join(format!("{rate}.wav"));
        write_wav(&path, 24, false, 2, rate);
        assert_eq!(decode(&path).unwrap().sample_rate, rate);
    }
    std::fs::remove_dir_all(dir).ok();
}

// -------------------------------------------------------------------- AIFF

/// An IEEE 754 80-bit extended of a whole number, as AIFF stores its sample rate.
fn extended(mut value: u32) -> [u8; 10] {
    let mut exponent = 16383 + 31;
    while value & 0x8000_0000 == 0 {
        value <<= 1;
        exponent -= 1;
    }
    let mut out = [0; 10];
    out[..2].copy_from_slice(&(exponent as u16).to_be_bytes());
    out[2..6].copy_from_slice(&value.to_be_bytes());
    out
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

fn write_aiff(path: &Path, compression: Option<(&[u8; 4], &str)>, bits: u16, data: &[u8]) {
    let frames = data.len() as u32 / 2 / (bits as u32 / 8);
    let mut comm = Vec::new();
    comm.extend_from_slice(&2i16.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&bits.to_be_bytes());
    comm.extend_from_slice(&extended(RATE));
    let mut form = Vec::new();
    if let Some((kind, name)) = compression {
        comm.extend_from_slice(kind);
        comm.push(name.len() as u8);
        comm.extend_from_slice(name.as_bytes());
        if !(name.len() + 1).is_multiple_of(2) {
            comm.push(0);
        }
        form.extend_from_slice(b"AIFC");
        chunk(&mut form, b"FVER", &0xA280_5140u32.to_be_bytes());
    } else {
        form.extend_from_slice(b"AIFF");
    }
    chunk(&mut form, b"COMM", &comm);
    let mut ssnd = vec![0; 8];
    ssnd.extend_from_slice(data);
    chunk(&mut form, b"SSND", &ssnd);
    let mut file = Vec::new();
    chunk(&mut file, b"FORM", &form);
    std::fs::write(path, file).unwrap();
}

fn interleave(bits: u32, little_endian: bool) -> Vec<u8> {
    let (l, r) = reference(bits);
    let mut out = Vec::new();
    for i in 0..FRAMES {
        for &v in &[l[i], r[i]] {
            let be = v.to_be_bytes();
            let mut bytes = be[4 - bits as usize / 8..].to_vec();
            if little_endian {
                bytes.reverse();
            }
            out.extend_from_slice(&bytes);
        }
    }
    out
}

#[test]
fn aiff_matches_the_reference() {
    let dir = temp_dir();
    for bits in [8u32, 16, 24, 32] {
        let path = dir.join(format!("{bits}.aiff"));
        write_aiff(&path, None, bits as u16, &interleave(bits, false));
        let a = decode(&path).unwrap();
        let (l, r) = reference(bits);
        assert_same(&a.left, &to_float(&l, bits), 2e-7, "left");
        assert_same(&a.right, &to_float(&r, bits), 2e-7, "right");
        assert_eq!(a.sample_rate, RATE, "{bits}");
        assert_eq!(probe(&path).unwrap().format, "aiff");
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn aiff_c_codecs_match_the_reference() {
    let dir = temp_dir();
    let cases: [(&[u8; 4], &str, u16, Vec<u8>); 3] = [
        (b"NONE", "not compressed", 24, interleave(24, false)),
        (b"sowt", "little-endian", 16, interleave(16, true)),
        (b"twos", "big-endian", 16, interleave(16, false)),
    ];
    for (kind, name, bits, data) in cases {
        let path = dir.join(format!("{}.aifc", String::from_utf8_lossy(kind)));
        write_aiff(&path, Some((kind, name)), bits, &data);
        let a = decode(&path).unwrap();
        let (l, r) = reference(bits.into());
        assert_same(&a.left, &to_float(&l, bits.into()), 2e-7, "left");
        assert_same(&a.right, &to_float(&r, bits.into()), 2e-7, "right");
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn aiff_c_floats_match_the_reference() {
    let dir = temp_dir();
    let (l, r) = reference(24);
    let (lf, rf) = (to_float(&l, 24), to_float(&r, 24));
    let data: Vec<u8> = lf
        .iter()
        .zip(&rf)
        .flat_map(|(a, b)| [a.to_be_bytes(), b.to_be_bytes()])
        .flatten()
        .collect();
    let path = dir.join("f.aifc");
    write_aiff(&path, Some((b"fl32", "32-bit float")), 32, &data);
    let a = decode(&path).unwrap();
    assert_same(&a.left, &lf, 0.0, "left");
    assert_same(&a.right, &rf, 0.0, "right");
    std::fs::remove_dir_all(dir).ok();
}

// -------------------------------------------------------------------- FLAC

struct Bits {
    bytes: Vec<u8>,
    filled: u32,
}

impl Bits {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            filled: 0,
        }
    }

    fn put(&mut self, value: u64, count: u32) {
        for i in (0..count).rev() {
            if self.filled == 0 {
                self.bytes.push(0);
            }
            let bit = ((value >> i) & 1) as u8;
            *self.bytes.last_mut().unwrap() |= bit << (7 - self.filled);
            self.filled = (self.filled + 1) % 8;
        }
    }

    fn align(&mut self) {
        while self.filled != 0 {
            self.put(0, 1);
        }
    }
}

fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |mut crc, b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
        crc
    })
}

fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |mut crc, b| {
        crc ^= (*b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// A FLAC file with every frame stored verbatim, which is valid if not very compact.
fn write_flac(path: &Path, bits: u32) {
    const BLOCK: usize = 256;
    let (l, r) = reference(bits);
    let mut out = b"fLaC".to_vec();
    out.push(0x80); // Last metadata block; STREAMINFO.
    out.extend_from_slice(&[0, 0, 34]);
    let mut info = Bits::new();
    info.put(BLOCK as u64, 16);
    info.put(BLOCK as u64, 16);
    info.put(0, 24);
    info.put(0, 24);
    info.put(RATE as u64, 20);
    info.put(1, 3);
    info.put(bits as u64 - 1, 5);
    info.put(FRAMES as u64, 36);
    out.extend_from_slice(&info.bytes);
    out.extend_from_slice(&[0; 16]);
    let size_code = match bits {
        16 => 0b100,
        24 => 0b110,
        _ => panic!("no code for {bits} bits"),
    };
    for (n, (cl, cr)) in l.chunks(BLOCK).zip(r.chunks(BLOCK)).enumerate() {
        let mut f = Bits::new();
        f.put(0b11111111111110, 14);
        f.put(0, 1);
        f.put(0, 1); // Fixed block size.
        f.put(0b0111, 4); // Block size in 16 bits after the header.
        f.put(0, 4); // Sample rate from STREAMINFO.
        f.put(0b0001, 4); // Left and right, independent.
        f.put(size_code, 3);
        f.put(0, 1);
        let mut number = [0; 4];
        for b in char::from_u32(n as u32)
            .unwrap()
            .encode_utf8(&mut number)
            .bytes()
        {
            f.put(b as u64, 8);
        }
        f.put(cl.len() as u64 - 1, 16);
        let header_crc = crc8(&f.bytes);
        f.put(header_crc as u64, 8);
        for channel in [cl, cr] {
            f.put(0b00000010, 8); // Verbatim subframe, no wasted bits.
            for &v in channel {
                f.put(v as u32 as u64 & ((1u64 << bits) - 1), bits);
            }
        }
        f.align();
        let crc = crc16(&f.bytes);
        f.put(crc as u64, 16);
        out.extend_from_slice(&f.bytes);
    }
    std::fs::write(path, out).unwrap();
}

#[test]
fn flac_matches_the_reference() {
    let dir = temp_dir();
    for bits in [16u32, 24] {
        let path = dir.join(format!("{bits}.flac"));
        write_flac(&path, bits);
        let info = probe(&path).unwrap();
        assert_eq!(info.format, "flac");
        assert_eq!(info.frames, Some(FRAMES as u64));
        assert!(info.is_compressed());
        let a = decode(&path).unwrap();
        let (l, r) = reference(bits);
        assert_same(&a.left, &to_float(&l, bits), 2e-7, "left");
        assert_same(&a.right, &to_float(&r, bits), 2e-7, "right");
        assert_eq!(a.sample_rate, RATE);
    }
    std::fs::remove_dir_all(dir).ok();
}

// -------------------------------------------------------------------- lossy fixtures

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The sine a fixture's channel was made from.
fn sine(hz: f64, amplitude: f64, rate: u32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (amplitude * (i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin()) as f32)
        .collect()
}

/// The worst difference between `got` and the reference, sliding it over `max_lag` samples to
/// absorb the codec's delay. Measured away from the ends, where codecs smear.
fn best_fit_error(got: &[f32], reference: &[f32], max_lag: usize) -> f32 {
    let margin = 2_400;
    let body = &reference[margin..reference.len() - margin];
    (0..=max_lag)
        .map(|lag| {
            body.iter()
                .enumerate()
                .map(|(i, want)| match got.get(margin + lag + i) {
                    Some(have) => (have - want).abs(),
                    None => want.abs(),
                })
                .fold(0.0f32, f32::max)
        })
        .fold(f32::MAX, f32::min)
}

fn check_lossy_stereo(name: &str, format: &str, tolerance: f32) -> DecodedAudio {
    let path = fixture(name);
    let info = probe(&path).unwrap();
    assert_eq!(info.format, format, "{name}");
    assert!(info.is_compressed(), "{name}");
    assert_eq!(info.sample_rate, 44_100, "{name}");
    let a = decode(&path).unwrap();
    assert_eq!(a.sample_rate, 44_100, "{name}");
    assert!(a.is_stereo(), "{name}");
    let n = RATE as usize / 2;
    assert!(
        a.frames().abs_diff(n) <= 4_096,
        "{name}: {} frames",
        a.frames()
    );
    let l = sine(440.0, 0.5, RATE, n);
    let r = sine(880.0, 0.25, RATE, n);
    assert!(
        best_fit_error(&a.left, &l, 4_096) < tolerance,
        "{name}: left off by {}",
        best_fit_error(&a.left, &l, 4_096)
    );
    assert!(
        best_fit_error(&a.right, &r, 4_096) < tolerance,
        "{name}: right off by {}",
        best_fit_error(&a.right, &r, 4_096)
    );
    let mid = &a.left[a.frames() / 4..a.frames() * 3 / 4];
    assert!((tone_amplitude(mid, 440.0, RATE as f64) - 0.5).abs() < 0.05);
    assert!(
        tone_amplitude(mid, 880.0, RATE as f64) < 0.05,
        "no crosstalk"
    );
    a
}

fn check_lossy_mono(name: &str, format: &str, tolerance: f32) {
    let path = fixture(name);
    let a = decode(&path).unwrap();
    assert_eq!(probe(&path).unwrap().format, format, "{name}");
    assert_eq!(a.sample_rate, 48_000, "{name}");
    assert!(!a.is_stereo(), "{name}");
    assert_eq!(a.left, a.right);
    let want = sine(440.0, 0.5, 48_000, 24_000);
    assert!(
        best_fit_error(&a.left, &want, 4_096) < tolerance,
        "{name}: off by {}",
        best_fit_error(&a.left, &want, 4_096)
    );
}

#[test]
fn ogg_vorbis_matches_the_sines() {
    check_lossy_stereo("stereo44.ogg", "ogg", 0.03);
    check_lossy_mono("mono48.ogg", "ogg", 0.03);
}

#[test]
fn mp3_matches_the_sines() {
    check_lossy_stereo("stereo44.mp3", "mp3", 0.03);
    check_lossy_mono("mono48.mp3", "mp3", 0.03);
}

#[test]
fn m4a_aac_matches_the_sines() {
    check_lossy_stereo("stereo44.m4a", "isomp4", 0.03);
    check_lossy_mono("mono48.m4a", "isomp4", 0.03);
}

// -------------------------------------------------------------------- bad files

#[test]
fn a_missing_file_says_so() {
    let dir = temp_dir();
    let err = decode(&dir.join("not-here.wav")).unwrap_err();
    assert!(matches!(err, DecodeError::NotFound), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    let dir = temp_dir();
    let path = dir.join("x.wav");
    std::fs::write(&path, vec![0x5Au8; 4096]).unwrap();
    assert!(decode(&path).is_err());
    std::fs::write(&path, b"").unwrap();
    assert!(decode(&path).is_err());
    let path = dir.join("wrong.flac");
    std::fs::write(&path, b"fLaC\0\0\0\0more").unwrap();
    assert!(decode(&path).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_truncated_wav_keeps_what_could_be_read() {
    let dir = temp_dir();
    let path = dir.join("cut.wav");
    write_wav(&path, 16, false, 2, RATE);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let a = decode(&path).unwrap();
    assert!(a.frames() > 0 && a.frames() < FRAMES, "{}", a.frames());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_file_with_a_wrong_extension_is_still_read() {
    let dir = temp_dir();
    let good = dir.join("a.wav");
    write_wav(&good, 16, false, 2, RATE);
    let path = dir.join("actually-a-wav.aiff");
    std::fs::rename(&good, &path).unwrap();
    assert_eq!(decode(&path).unwrap().frames(), FRAMES);
    std::fs::remove_dir_all(dir).ok();
}
