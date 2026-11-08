//! Launching and stopping Session clips.

use crate::{clip, retire, Engine, Garbage};
use rlive_core::TrackId;

pub enum LaunchCmd {
    LaunchClip {
        track: TrackId,
        slot: usize,
    },
    StopTrack(TrackId),
    LaunchScene(usize),
    StopAllClips,
    /// Put every track that launched a Session clip back on its lane.
    BackToArrangement,
}

impl Engine {
    pub(crate) fn handle_launch(&mut self, cmd: LaunchCmd) {
        use LaunchCmd as C;
        match cmd {
            C::LaunchClip { track, slot } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.launch(i, slot);
                }
            }
            C::StopTrack(track) => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.stop_track(i);
                }
            }
            C::LaunchScene(slot) => {
                for i in 0..self.tracks.len() {
                    self.launch(i, slot);
                }
            }
            C::StopAllClips => {
                for i in 0..self.tracks.len() {
                    self.stop_track(i);
                }
            }
            C::BackToArrangement => {
                for t in self.tracks.iter_mut() {
                    t.player.pending = None;
                    if !t.follow {
                        t.player.release_all(0, &mut t.live);
                        if let Some(p) = t.player.playing.take() {
                            retire(&mut self.garbage, Garbage::Shared(p.clip));
                        }
                        t.follow = true;
                        t.lane.release_and_resync(0, &mut t.live);
                    }
                }
            }
        }
    }

    fn launch(&mut self, track_idx: usize, slot: usize) {
        self.start_transport();
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        let has_clip = matches!(track.slots.get(slot), Some(Some(_)));
        track.player.pending = Some(if has_clip {
            clip::Pending::Launch { slot, at }
        } else {
            clip::Pending::Stop { at }
        });
    }

    fn stop_track(&mut self, track_idx: usize) {
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        if track.player.playing.is_some() || track.player.pending.is_some() {
            track.player.pending = Some(clip::Pending::Stop { at });
        }
    }
}
