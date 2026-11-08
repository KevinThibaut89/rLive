//! Launching and stopping Session clips.

use crate::capture::SlotRec;
use crate::{clip, retire, Engine, EngineEvent, Garbage};
use rlive_core::TrackId;
use std::sync::atomic::Ordering::Relaxed;

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
    /// Start recording into an empty slot, at the next launch point.
    RecordSlot {
        track: TrackId,
        slot: usize,
    },
    /// End a slot's recording at the next launch point and play the new clip from there.
    CloseRecording {
        track: TrackId,
    },
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
            C::RecordSlot { track, slot } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.record_slot(i, slot);
                }
            }
            C::CloseRecording { track } => {
                if let Some(i) = self.tracks.iter().position(|t| t.id == track) {
                    self.close_recording(i, true);
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
        if let Some(rec) = self.tracks[track_idx].slot_rec {
            if rec.close.is_none() {
                self.close_recording(track_idx, rec.slot == slot);
                if rec.slot == slot {
                    return;
                }
            }
        }
        let track = &mut self.tracks[track_idx];
        let has_clip = matches!(track.slots.get(slot), Some(Some(_)));
        track.player.pending = Some(if has_clip {
            clip::Pending::Launch { slot, at }
        } else {
            clip::Pending::Stop { at }
        });
    }

    fn stop_track(&mut self, track_idx: usize) {
        if self.tracks[track_idx]
            .slot_rec
            .is_some_and(|r| r.close.is_none())
        {
            self.close_recording(track_idx, false);
            return;
        }
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        if track.player.playing.is_some() || track.player.pending.is_some() {
            track.player.pending = Some(clip::Pending::Stop { at });
        }
    }

    fn record_slot(&mut self, track_idx: usize, slot: usize) {
        let from = self.launch_point();
        let track = &mut self.tracks[track_idx];
        if track.slot_rec.is_some() {
            return;
        }
        track.slot_rec = Some(SlotRec {
            slot,
            from,
            started: false,
            close: None,
        });
        track.shared.record_queued_slot.store(slot as i32, Relaxed);
        if track.player.playing.is_some() || track.player.pending.is_some() {
            track.player.pending = Some(clip::Pending::Stop { at: from });
        }
        // After the track is waiting, so a stopped transport waits for the count-in with it.
        self.start_transport();
    }

    /// End a slot's recording at the next launch point; with `launch` its clip plays from there.
    fn close_recording(&mut self, track_idx: usize, launch: bool) {
        let at = self.launch_point();
        let track = &mut self.tracks[track_idx];
        let Some(rec) = track.slot_rec.as_mut() else {
            return;
        };
        if rec.close.is_some() {
            return;
        }
        // Closing before it has started records nothing, and the slot stays empty.
        let at = at.max(rec.from);
        rec.close = Some((at, launch));
        let slot = rec.slot;
        track.player.pending = Some(if launch {
            clip::Pending::Launch { slot, at }
        } else {
            clip::Pending::Stop { at }
        });
        track.player.hold_for_arrival = launch;
        let frames = track.capture.as_ref().map(|c| c.frames());
        let id = track.id;
        let tps = self.tempo_run().ticks_per_sample;
        let frames_until = ((at - self.tick) / tps).max(0.0).round() as u64;
        let _ = self.events.push(EngineEvent::SlotClosing {
            track: id,
            slot,
            at,
            launch,
            frames: frames.map(|f| f + frames_until),
        });
    }
}
