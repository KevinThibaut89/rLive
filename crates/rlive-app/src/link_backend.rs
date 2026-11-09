//! Ableton Link, when the build has it.
//!
//! The Link SDK is GPL-2.0-or-later, so it only comes in with the `link` cargo feature. Without
//! the feature there is no session to connect, and nothing of the SDK is compiled.

use rlive_engine::sync::LinkClock;

/// Whether this build can join a Link session.
pub fn available() -> bool {
    cfg!(feature = "link")
}

/// Start a Link session at `bpm`, or `None` when this build has no Link.
#[cfg(feature = "link")]
pub fn connect(bpm: f64, start_stop_sync: bool) -> Option<Box<dyn LinkClock>> {
    Some(Box::new(backend::RustyLink::new(bpm, start_stop_sync)))
}

/// Start a Link session at `bpm`, or `None` when this build has no Link.
#[cfg(not(feature = "link"))]
pub fn connect(_bpm: f64, _start_stop_sync: bool) -> Option<Box<dyn LinkClock>> {
    None
}

#[cfg(feature = "link")]
mod backend {
    use rlive_engine::sync::{LinkClock, LinkState};
    use rusty_link::{AblLink, SessionState};

    pub struct RustyLink {
        link: AblLink,
        session: SessionState,
        start_stop_sync: bool,
    }

    // `AblLink` and `SessionState` wrap the SDK's handles; the audio-thread calls used here are
    // the ones the SDK documents as safe from a real-time thread, and nothing is shared.
    unsafe impl Send for RustyLink {}

    impl RustyLink {
        pub fn new(bpm: f64, start_stop_sync: bool) -> Self {
            let link = AblLink::new(bpm);
            link.enable(true);
            link.enable_start_stop_sync(start_stop_sync);
            let mut session = SessionState::new();
            link.capture_app_session_state(&mut session);
            Self {
                link,
                session,
                start_stop_sync,
            }
        }
    }

    impl LinkClock for RustyLink {
        fn capture(&mut self, quantum: f64, latency: f64) -> LinkState {
            self.link.capture_audio_session_state(&mut self.session);
            let time = self.link.clock_micros() + (latency * 1e6) as i64;
            LinkState {
                tempo: self.session.tempo(),
                phase: self.session.phase_at_time(time, quantum),
                peers: self.link.num_peers() as u32,
                playing: self.start_stop_sync.then(|| self.session.is_playing()),
            }
        }

        fn set_tempo(&mut self, bpm: f64) {
            let time = self.link.clock_micros();
            self.session.set_tempo(bpm, time);
            self.link.commit_audio_session_state(&self.session);
        }

        fn set_playing(&mut self, playing: bool) {
            let time = self.link.clock_micros();
            self.session.set_is_playing(playing, time);
            self.link.commit_audio_session_state(&self.session);
        }
    }
}

#[cfg(all(test, feature = "link"))]
mod tests {
    use super::*;

    /// The SDK itself, on this machine, with nobody else around.
    #[test]
    fn a_session_with_no_peers_reports_our_tempo() {
        let mut link = connect(123.0, false).expect("this build has Link");
        let state = link.capture(4.0, 0.0);
        assert_eq!(state.peers, 0);
        assert!((state.tempo - 123.0).abs() < 1e-6);
        link.set_tempo(99.5);
        let state = link.capture(4.0, 0.0);
        assert!((state.tempo - 99.5).abs() < 1e-6);
        assert!((0.0..4.0).contains(&state.phase));
    }
}
