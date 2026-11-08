//! Writing audio files as they are recorded.
//!
//! Sizes are unknown until the end, so a header goes down with room for them and is patched when
//! the file is finished. A file that is never finished still has all its audio in it, only a
//! header that claims less.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// The file a take is recorded into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FileType {
    #[default]
    Wav,
    Aiff,
}

impl FileType {
    pub const ALL: [FileType; 2] = [FileType::Wav, FileType::Aiff];

    pub fn label(self) -> &'static str {
        match self {
            FileType::Wav => "WAV",
            FileType::Aiff => "AIFF",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            FileType::Wav => "wav",
            FileType::Aiff => "aif",
        }
    }
}

/// How finely samples are stored. 32 is float in WAV and integer in AIFF, which has no float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BitDepth {
    Bits16,
    #[default]
    Bits24,
    Bits32,
}

impl BitDepth {
    pub const ALL: [BitDepth; 3] = [BitDepth::Bits16, BitDepth::Bits24, BitDepth::Bits32];

    pub fn label(self) -> &'static str {
        match self {
            BitDepth::Bits16 => "16",
            BitDepth::Bits24 => "24",
            BitDepth::Bits32 => "32",
        }
    }

    pub fn bits(self) -> u16 {
        match self {
            BitDepth::Bits16 => 16,
            BitDepth::Bits24 => 24,
            BitDepth::Bits32 => 32,
        }
    }

    fn bytes(self) -> usize {
        self.bits() as usize / 8
    }
}

/// A recording going to disk.
pub struct AudioWriter {
    file: BufWriter<File>,
    kind: FileType,
    depth: BitDepth,
    channels: u16,
    samples: u64,
    /// A frame only part-delivered: samples may arrive split anywhere.
    scratch: Vec<u8>,
}

/// Where the fields that need patching at the end are.
const AIFF_FORM_SIZE: u64 = 4;
const AIFF_COMM_FRAMES: u64 = 22;
const AIFF_SSND_SIZE: u64 = 42;
const WAV_RIFF_SIZE: u64 = 4;
const WAV_DATA_SIZE: u64 = 40;

impl AudioWriter {
    pub fn create(
        path: &Path,
        kind: FileType,
        depth: BitDepth,
        channels: u16,
        sample_rate: u32,
    ) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = BufWriter::with_capacity(1 << 20, File::create(path)?);
        match kind {
            FileType::Wav => write_wav_header(&mut file, depth, channels, sample_rate)?,
            FileType::Aiff => write_aiff_header(&mut file, depth, channels, sample_rate)?,
        }
        Ok(Self {
            file,
            kind,
            depth,
            channels,
            samples: 0,
            scratch: Vec::with_capacity(4096),
        })
    }

    /// Frames written so far, whole or in part.
    pub fn frames(&self) -> u64 {
        self.samples / self.channels.max(1) as u64
    }

    /// Writes interleaved samples, in -1..1.
    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.scratch.clear();
        for &sample in samples {
            self.scratch
                .extend_from_slice(&encode(sample, self.kind, self.depth).0[..self.depth.bytes()]);
        }
        self.file.write_all(&self.scratch)?;
        self.samples += samples.len() as u64;
        Ok(())
    }

    /// Writes this many frames of silence.
    pub fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        let zero = [0u8; 4];
        for _ in 0..frames * self.channels as u64 {
            self.file.write_all(&zero[..self.depth.bytes()])?;
        }
        self.samples += frames * self.channels as u64;
        Ok(())
    }

    /// Fixes the header up and closes the file. Returns its length in frames.
    pub fn finish(mut self) -> io::Result<u64> {
        let frames = self.frames();
        let data = self.samples * self.depth.bytes() as u64;
        let pad = data % 2;
        if pad == 1 {
            self.file.write_all(&[0])?;
        }
        let total = data + pad;
        match self.kind {
            FileType::Wav => {
                self.patch_le(WAV_RIFF_SIZE, (total + 36) as u32)?;
                self.patch_le(WAV_DATA_SIZE, data as u32)?;
            }
            FileType::Aiff => {
                self.patch_be(AIFF_FORM_SIZE, (46 + total) as u32)?;
                self.patch_be(AIFF_COMM_FRAMES, frames as u32)?;
                self.patch_be(AIFF_SSND_SIZE, (data + 8) as u32)?;
            }
        }
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(frames)
    }

    fn patch_le(&mut self, at: u64, value: u32) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(at))?;
        self.file.write_all(&value.to_le_bytes())
    }

    fn patch_be(&mut self, at: u64, value: u32) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(at))?;
        self.file.write_all(&value.to_be_bytes())
    }
}

/// A sample as bytes, in the file's byte order.
fn encode(sample: f32, kind: FileType, depth: BitDepth) -> ([u8; 4], usize) {
    let s = sample.clamp(-1.0, 1.0);
    let n = depth.bytes();
    if depth == BitDepth::Bits32 && kind == FileType::Wav {
        return (sample.to_le_bytes(), n);
    }
    let value = match depth {
        BitDepth::Bits16 => (s as f64 * i16::MAX as f64).round() as i32 as u32 & 0xffff,
        BitDepth::Bits24 => (s as f64 * 8_388_607.0).round() as i32 as u32 & 0xff_ffff,
        BitDepth::Bits32 => (s as f64 * i32::MAX as f64).round() as i32 as u32,
    };
    let mut bytes = [0u8; 4];
    match kind {
        FileType::Wav => bytes.copy_from_slice(&value.to_le_bytes()),
        FileType::Aiff => {
            let be = value.to_be_bytes();
            bytes[..n].copy_from_slice(&be[4 - n..]);
        }
    }
    (bytes, n)
}

fn write_wav_header(
    out: &mut impl Write,
    depth: BitDepth,
    channels: u16,
    sample_rate: u32,
) -> io::Result<()> {
    let align = channels * depth.bytes() as u16;
    let format: u16 = if depth == BitDepth::Bits32 { 3 } else { 1 };
    out.write_all(b"RIFF\0\0\0\0WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&format.to_le_bytes())?;
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&sample_rate.to_le_bytes())?;
    out.write_all(&(sample_rate * align as u32).to_le_bytes())?;
    out.write_all(&align.to_le_bytes())?;
    out.write_all(&depth.bits().to_le_bytes())?;
    out.write_all(b"data\0\0\0\0")
}

fn write_aiff_header(
    out: &mut impl Write,
    depth: BitDepth,
    channels: u16,
    sample_rate: u32,
) -> io::Result<()> {
    out.write_all(b"FORM\0\0\0\0AIFFCOMM")?;
    out.write_all(&18u32.to_be_bytes())?;
    out.write_all(&channels.to_be_bytes())?;
    out.write_all(&0u32.to_be_bytes())?;
    out.write_all(&depth.bits().to_be_bytes())?;
    out.write_all(&extended80(sample_rate))?;
    out.write_all(b"SSND\0\0\0\0")?;
    out.write_all(&0u32.to_be_bytes())?;
    out.write_all(&0u32.to_be_bytes())
}

/// An integer sample rate as AIFF's 80-bit extended float.
fn extended80(value: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if value == 0 {
        return out;
    }
    let shift = value.leading_zeros();
    let mantissa = ((value as u64) << shift) << 32;
    let exponent = 16383 + 31 - shift as u16;
    out[..2].copy_from_slice(&exponent.to_be_bytes());
    out[2..].copy_from_slice(&mantissa.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rlive-writer-{}-{name}", std::process::id()))
    }

    fn roundtrip(kind: FileType, depth: BitDepth, channels: u16) {
        let name = format!("{kind:?}-{depth:?}-{channels}.{}", kind.extension());
        let p = path(&name);
        let mut w = AudioWriter::create(&p, kind, depth, channels, 44_100).unwrap();
        let mut samples = Vec::new();
        for i in 0..1000 {
            samples.push(i as f32 / 2000.0 - 0.25);
            if channels == 2 {
                samples.push(-0.5);
            }
        }
        w.write(&samples).unwrap();
        w.write_silence(10).unwrap();
        assert_eq!(w.finish().unwrap(), 1010);
        let decoded = decode(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(decoded.sample_rate, 44_100, "{name}");
        assert_eq!(decoded.frames(), 1010, "{name}");
        assert!((decoded.left[500] - 0.0).abs() < 1e-3, "{name}");
        assert_eq!(decoded.left[1005], 0.0, "{name}: the gap is silence");
    }

    #[test]
    fn every_type_and_depth_reads_back() {
        for kind in FileType::ALL {
            for depth in BitDepth::ALL {
                roundtrip(kind, depth, 1);
                roundtrip(kind, depth, 2);
            }
        }
    }

    #[test]
    fn sample_rate_is_stored_as_an_extended_float() {
        assert_eq!(
            extended80(44_100),
            [0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]
        );
    }
}
