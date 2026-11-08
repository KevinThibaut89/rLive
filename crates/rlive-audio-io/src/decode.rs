//! Decoding audio files with Symphonia.

use std::fs::File;
use std::path::{Path, PathBuf};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("file not found")]
    NotFound,
    #[error("could not read the file: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a supported audio file: {0}")]
    Unsupported(String),
    #[error("the file has no audio")]
    NoAudio,
    #[error("could not decode the file: {0}")]
    Corrupt(String),
}

/// Consecutive undecodable packets after which a file counts as corrupt.
const MAX_BAD_PACKETS: usize = 32;
/// Frames reserved up front from the file's own claim of its length.
const MAX_RESERVED_FRAMES: u64 = 1 << 26;

/// What a file's header says about it.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInfo {
    /// The container's short name, such as `"mp3"` or `"wave"`.
    pub format: String,
    pub sample_rate: u32,
    /// Channels in the file. Only the first two are ever read.
    pub channels: usize,
    /// Frames, if the container states them. An estimate for some compressed formats.
    pub frames: Option<u64>,
    pub bits_per_sample: Option<u32>,
}

impl AudioInfo {
    /// Whether the file needs real decoding work, and so belongs in the decoding cache.
    pub fn is_compressed(&self) -> bool {
        !matches!(self.format.as_str(), "wave" | "aiff")
    }

    pub fn duration_secs(&self) -> Option<f64> {
        let frames = self.frames?;
        (self.sample_rate > 0).then(|| frames as f64 / self.sample_rate as f64)
    }
}

/// A decoded file. `right` equals `left` when the file is mono.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: u32,
    /// Channels in the source, capped at 2.
    pub channels: usize,
    /// The stream ended before its header said it would.
    pub truncated: bool,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn is_stereo(&self) -> bool {
        self.channels > 1
    }
}

/// Reads a file's format without decoding it. Touches only the start of the file.
pub fn probe(path: &Path) -> Result<AudioInfo, DecodeError> {
    Ok(Decoder::open(path)?.info().clone())
}

/// A file being decoded, block by block.
///
/// This is the seam for disk streaming: a reader that streams long files holds one of these and
/// calls [`next_block`](Self::next_block) as it needs audio, where [`decode_all`](Self::decode_all)
/// collects everything into memory.
pub struct Decoder {
    path: PathBuf,
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    info: AudioInfo,
    planes: Vec<Vec<f32>>,
    good_packets: usize,
    bad_in_a_row: usize,
    truncated: bool,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DecodeError::NotFound
            } else {
                DecodeError::Io(e)
            }
        })?;
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(
                &hint,
                MediaSourceStream::new(Box::new(file), Default::default()),
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
        let format = reader.format_info().short_name.to_string();
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::NoAudio)?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(DecodeError::NoAudio)?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
        let info = AudioInfo {
            format,
            sample_rate: params.sample_rate.unwrap_or(0),
            channels: params.channels.as_ref().map_or(0, |c| c.count()),
            frames: track.num_frames,
            bits_per_sample: params.bits_per_sample,
        };
        Ok(Self {
            path: path.to_path_buf(),
            track: track.id,
            reader,
            decoder,
            info,
            planes: Vec::new(),
            good_packets: 0,
            bad_in_a_row: 0,
            truncated: false,
        })
    }

    pub fn info(&self) -> &AudioInfo {
        &self.info
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends the next block of audio to `left` and `right`. Returns `false` at the end.
    pub fn next_block(
        &mut self,
        left: &mut Vec<f32>,
        right: &mut Vec<f32>,
    ) -> Result<bool, DecodeError> {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                // A chained stream: only its first part is read.
                Err(SymphoniaError::ResetRequired) => return Ok(false),
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.truncated = true;
                    return Ok(false);
                }
                Err(e) => return Err(DecodeError::Corrupt(e.to_string())),
            };
            if packet.track_id != self.track {
                continue;
            }
            let block = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(SymphoniaError::DecodeError(e)) => {
                    log::debug!("{}: skipped a packet: {e}", self.path.display());
                    self.bad_in_a_row += 1;
                    if self.bad_in_a_row > MAX_BAD_PACKETS {
                        return Err(DecodeError::Corrupt(e.to_string()));
                    }
                    continue;
                }
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.truncated = true;
                    return Ok(false);
                }
                Err(e) => return Err(DecodeError::Corrupt(e.to_string())),
            };
            self.bad_in_a_row = 0;
            self.good_packets += 1;
            self.info.sample_rate = block.spec().rate();
            self.info.channels = block.spec().channels().count();
            if self.info.channels == 0 || block.frames() == 0 {
                continue;
            }
            block.copy_to_vecs_planar::<f32>(&mut self.planes);
            left.extend_from_slice(&self.planes[0]);
            right.extend_from_slice(self.planes.get(1).unwrap_or(&self.planes[0]));
            return Ok(true);
        }
    }

    /// Decodes everything into memory.
    pub fn decode_all(mut self) -> Result<DecodedAudio, DecodeError> {
        // A header can lie, so what it says is only trusted up to a point.
        let capacity = self.info.frames.unwrap_or(0).min(MAX_RESERVED_FRAMES) as usize;
        let mut left = Vec::with_capacity(capacity);
        let mut right = Vec::with_capacity(capacity);
        while self.next_block(&mut left, &mut right)? {}
        if self.good_packets == 0 || left.is_empty() {
            return Err(DecodeError::NoAudio);
        }
        if self.truncated {
            log::warn!(
                "{}: the file ends early; using what could be read",
                self.path.display()
            );
        }
        Ok(DecodedAudio {
            left,
            right,
            sample_rate: self.info.sample_rate,
            channels: self.info.channels.min(2),
            truncated: self.truncated,
        })
    }
}

/// Decodes a whole file into memory, on the calling thread.
pub fn decode(path: &Path) -> Result<DecodedAudio, DecodeError> {
    Decoder::open(path)?.decode_all()
}
