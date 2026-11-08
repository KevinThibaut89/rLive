//! Adding, removing and reordering tracks and devices.

use crate::{retire, ChainTarget, DeviceSlot, Engine, Garbage, MasterNode, TrackNode};
use rlive_core::{DeviceId, TrackId};

pub enum GraphCmd {
    AddTrack {
        index: usize,
        track: Box<TrackNode>,
    },
    RemoveTrack(TrackId),
    MoveTrack {
        track: TrackId,
        to: usize,
    },
    InsertDevice {
        target: ChainTarget,
        index: usize,
        device: Box<DeviceSlot>,
    },
    RemoveDevice {
        target: ChainTarget,
        device: DeviceId,
    },
    MoveDevice {
        target: ChainTarget,
        device: DeviceId,
        to: usize,
    },
    /// Replace the whole session (project load / sample-rate change).
    ReplaceAll {
        tracks: Box<Vec<Box<TrackNode>>>,
        master: Box<MasterNode>,
    },
}

impl Engine {
    pub(crate) fn handle_graph(&mut self, cmd: GraphCmd) {
        use GraphCmd as C;
        match cmd {
            C::AddTrack { index, track } => {
                if self.tracks.len() < self.tracks.capacity() {
                    let idx = index.min(self.tracks.len());
                    self.tracks.insert(idx, track);
                } else {
                    retire(&mut self.garbage, Garbage::Boxed(track));
                }
            }
            C::RemoveTrack(id) => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == id) {
                    let mut t = self.tracks.remove(i);
                    for d in t.devices.iter_mut() {
                        d.processor.stop_processing();
                    }
                    if let Some(p) = t.player.playing.take() {
                        retire(&mut self.garbage, Garbage::Shared(p.clip));
                    }
                    retire(&mut self.garbage, Garbage::Boxed(t));
                }
            }
            C::MoveTrack { track, to } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    let t = self.tracks.remove(i);
                    let to = to.min(self.tracks.len());
                    self.tracks.insert(to, t);
                }
            }
            C::InsertDevice {
                target,
                index,
                device,
            } => match self.chain_mut(target) {
                Some(chain) if chain.len() < chain.capacity() => {
                    let idx = index.min(chain.len());
                    chain.insert(idx, device);
                }
                _ => retire(&mut self.garbage, Garbage::Boxed(device)),
            },
            C::RemoveDevice { target, device } => {
                let removed = self.chain_mut(target).and_then(|chain| {
                    let i = chain.iter().position(|d| d.id == device)?;
                    Some(chain.remove(i))
                });
                if let Some(mut slot) = removed {
                    slot.processor.stop_processing();
                    retire(&mut self.garbage, Garbage::Boxed(slot));
                }
            }
            C::MoveDevice { target, device, to } => {
                if let Some(chain) = self.chain_mut(target) {
                    if let Some(i) = chain.iter().position(|d| d.id == device) {
                        let d = chain.remove(i);
                        let to = to.min(chain.len());
                        chain.insert(to, d);
                    }
                }
            }
            C::ReplaceAll {
                mut tracks,
                mut master,
            } => {
                for t in self.tracks.iter_mut() {
                    for d in t.devices.iter_mut() {
                        d.processor.stop_processing();
                    }
                    if let Some(p) = t.player.playing.take() {
                        retire(&mut self.garbage, Garbage::Shared(p.clip));
                    }
                }
                for d in self.master.devices.iter_mut() {
                    d.processor.stop_processing();
                }
                std::mem::swap(&mut self.tracks, &mut *tracks);
                std::mem::swap(&mut self.master, &mut master);
                retire(&mut self.garbage, Garbage::Boxed(tracks));
                retire(&mut self.garbage, Garbage::Boxed(master));
                self.playing = false;
                self.tick = 0.0;
                self.sample_pos = 0;
                self.insert_marker = 0.0;
                self.loop_range = None;
                self.reached_loop_end = false;
                self.recording = false;
            }
        }
    }
}
