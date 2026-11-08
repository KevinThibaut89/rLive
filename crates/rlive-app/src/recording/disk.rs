//! The thread that gets recorded audio to disk.
//!
//! It takes samples off each take's capture ring every few milliseconds and writes them out. The
//! audio thread only ever talks to the ring, so nothing here can hold it up: if the disk is too
//! slow, the ring fills and the audio thread leaves blocks out, which this thread then fills
//! with silence so the file stays in step with the timeline.

use rlive_audio_io::{AudioWriter, BitDepth, FileType};
use rlive_engine::{CaptureChunk, CaptureTail};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the thread looks at its rings.
const POLL: Duration = Duration::from_millis(4);

/// Audio kept in memory as well as going to disk, for a Session take that has to be playable
/// the moment it closes. A file on disk only appears complete once it's closed.
#[derive(Default)]
pub struct Mirror {
    /// One per channel, each as long as the take so far.
    pub channels: Mutex<[Vec<f32>; 2]>,
}

pub struct TakeJob {
    pub id: u64,
    pub path: PathBuf,
    pub tail: CaptureTail,
    pub file_type: FileType,
    pub bit_depth: BitDepth,
    pub sample_rate: u32,
    pub mirror: Option<Arc<Mirror>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiskEvent {
    /// The take's file is complete.
    Finished {
        id: u64,
        path: PathBuf,
        frames: u64,
        dropped: u64,
    },
    /// The take's file could not be written.
    Failed { id: u64, reason: String },
}

struct Running {
    id: u64,
    path: PathBuf,
    tail: CaptureTail,
    writer: AudioWriter,
    mirror: Option<Arc<Mirror>>,
    /// Which channel the next sample of a part-delivered frame belongs to.
    next_channel: usize,
}

impl Running {
    fn drain(&mut self) -> Result<usize, String> {
        let (writer, mirror, next) = (&mut self.writer, &self.mirror, &mut self.next_channel);
        let channels = self.tail.channels() as usize;
        let mut result = Ok(());
        let n = self.tail.drain(|chunk| {
            if result.is_err() {
                return;
            }
            match chunk {
                CaptureChunk::Samples(samples) => {
                    result = writer.write(samples);
                    if let Some(m) = mirror {
                        let mut buffers = m.channels.lock().unwrap();
                        for &s in samples {
                            buffers[*next].push(s);
                            *next = (*next + 1) % channels;
                        }
                    }
                }
                CaptureChunk::Gap(frames) => {
                    result = writer.write_silence(frames);
                    if let Some(m) = mirror {
                        let mut buffers = m.channels.lock().unwrap();
                        for buffer in buffers.iter_mut().take(channels) {
                            buffer.resize(buffer.len() + frames as usize, 0.0);
                        }
                    }
                }
            }
        });
        result.map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(n)
    }
}

enum Message {
    Start(Box<TakeJob>),
}

/// The disk thread, and how to talk to it.
pub struct DiskWriter {
    jobs: Sender<Message>,
    pub events: Receiver<DiskEvent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for DiskWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl DiskWriter {
    pub fn new() -> Self {
        let (jobs, inbox) = channel();
        let (events, outbox) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("rlive-disk-writer".into())
                .spawn(move || run(inbox, events, stop))
                .expect("spawn disk writer")
        };
        Self {
            jobs,
            events: outbox,
            stop,
            thread: Some(thread),
        }
    }

    pub fn start(&self, job: TakeJob) {
        let _ = self.jobs.send(Message::Start(Box::new(job)));
    }
}

impl Default for DiskWriter {
    fn default() -> Self {
        Self::new()
    }
}

fn run(inbox: Receiver<Message>, events: Sender<DiskEvent>, stop: Arc<AtomicBool>) {
    let mut running: Vec<Running> = Vec::new();
    while !stop.load(Ordering::Relaxed) || !running.is_empty() {
        while let Ok(Message::Start(job)) = inbox.try_recv() {
            let job = *job;
            match AudioWriter::create(
                &job.path,
                job.file_type,
                job.bit_depth,
                job.tail.channels() as u16,
                job.sample_rate,
            ) {
                Ok(writer) => running.push(Running {
                    id: job.id,
                    path: job.path,
                    tail: job.tail,
                    writer,
                    mirror: job.mirror,
                    next_channel: 0,
                }),
                Err(e) => {
                    let _ = events.send(DiskEvent::Failed {
                        id: job.id,
                        reason: format!("{}: {e}", job.path.display()),
                    });
                }
            }
        }
        let mut still = Vec::with_capacity(running.len());
        for mut job in running.drain(..) {
            match job.drain() {
                Err(reason) => {
                    let _ = events.send(DiskEvent::Failed { id: job.id, reason });
                }
                // Abandoned first, then drained again: nothing can have been missed in between.
                Ok(_) if job.tail.is_abandoned() => {
                    if let Err(reason) = job.drain() {
                        let _ = events.send(DiskEvent::Failed { id: job.id, reason });
                        continue;
                    }
                    let Running {
                        id,
                        path,
                        writer,
                        tail,
                        ..
                    } = job;
                    let dropped = tail.dropped();
                    // What never got a marker into the ring is at the very end.
                    let mut writer = writer;
                    let missing = tail.expected_frames().saturating_sub(writer.frames());
                    let ended = writer.write_silence(missing).and_then(|()| writer.finish());
                    match ended {
                        Ok(frames) => {
                            let _ = events.send(DiskEvent::Finished {
                                id,
                                path,
                                frames,
                                dropped,
                            });
                        }
                        Err(e) => {
                            let _ = events.send(DiskEvent::Failed {
                                id,
                                reason: format!("{}: {e}", path.display()),
                            });
                        }
                    }
                }
                Ok(_) => still.push(job),
            }
        }
        running = still;
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlive_audio_io::decode::decode;
    use rlive_engine::capture::CaptureMode;
    use rlive_engine::{capture_pair, CaptureSink};

    /// Feeds a sink as the audio thread would, one block at a time. `run` is crate-private to the
    /// engine, so the file side is exercised through a recording session in `takes_tests`; here
    /// the writer's own behaviour is checked with a sink that has already gone.
    fn finished(tail: CaptureTail) -> (PathBuf, Vec<DiskEvent>) {
        let path = std::env::temp_dir().join(format!(
            "rlive-disk-{}-{:?}.wav",
            std::process::id(),
            std::thread::current().id()
        ));
        let writer = DiskWriter::new();
        writer.start(TakeJob {
            id: 7,
            path: path.clone(),
            tail,
            file_type: FileType::Wav,
            bit_depth: BitDepth::Bits16,
            sample_rate: 48_000,
            mirror: None,
        });
        let event = writer.events.recv_timeout(Duration::from_secs(5)).unwrap();
        (path, vec![event])
    }

    #[test]
    fn an_empty_take_finishes_with_an_empty_file() {
        let (sink, tail): (CaptureSink, CaptureTail) =
            capture_pair(1, 32, CaptureMode::Arrangement, 0);
        drop(sink);
        let (path, events) = finished(tail);
        assert!(matches!(events[0], DiskEvent::Finished { frames: 0, .. }));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            44,
            "a header and no audio"
        );
        assert!(decode(&path).is_err(), "nothing there to play");
        std::fs::remove_file(path).ok();
    }
}
