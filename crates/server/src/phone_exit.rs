//! The person at the phone ended the session.
//!
//! iOS ends UI automation when someone holds both volume keys, and a runner
//! start waits on a passcode prompt the person can cancel. Both used to look
//! like a crash to the KeepAlive supervisor, which rebuilt the runner at once:
//! the phone went back into automation mode (and, with a passcode, asked for
//! it again) right after the person had said no.
//!
//! Now the supervisor parks instead and leaves [`PARKED_FILE`]; the daemon
//! sees it and releases the phone the way idle release does. Nothing brings
//! the runner back on its own (no rebuild, no pre-warm): the next request
//! does. A cancelled passcode prompt is a plainer no, so it also leaves
//! [`HANDOFF_FILE`], the persistent form of `{"mode":"human"}`: agent input
//! is refused until someone takes the phone back (`{"mode":"agent"}`, the
//! web panel's 「交给 agent」, or `iphone-use resume`).

use std::path::Path;

/// Written by the supervisor when the runner ended from the phone side.
/// Holds the [`Reason`]. Removed when the phone is next brought up.
pub const PARKED_FILE: &str = "runner-parked";

/// The phone is handed to the person holding it, across daemon restarts.
pub const HANDOFF_FILE: &str = "human-handoff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The runner exited while the phone stayed connected and unlocked
    /// (most often the volume-key exit).
    Exited,
    /// The person cancelled the passcode prompt for UI automation.
    PasscodeCanceled,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::PasscodeCanceled => "passcode_canceled",
        }
    }
}

/// Record a phone-side end. A cancelled passcode also hands the phone over.
pub fn park(state_dir: &Path, reason: Reason) {
    let _ = std::fs::write(state_dir.join(PARKED_FILE), format!("{}\n", reason.as_str()));
    if reason == Reason::PasscodeCanceled {
        set_handoff(state_dir, true);
    }
}

pub fn parked(state_dir: &Path) -> bool {
    state_dir.join(PARKED_FILE).exists()
}

pub fn clear_parked(state_dir: &Path) {
    let _ = std::fs::remove_file(state_dir.join(PARKED_FILE));
}

pub fn handoff(state_dir: &Path) -> bool {
    state_dir.join(HANDOFF_FILE).exists()
}

pub fn set_handoff(state_dir: &Path, active: bool) {
    let path = state_dir.join(HANDOFF_FILE);
    if active {
        let _ = std::fs::write(path, b"human\n");
    } else {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exit_parks_without_handing_over() {
        let dir = tempfile::tempdir().unwrap();
        park(dir.path(), Reason::Exited);
        assert!(parked(dir.path()));
        assert!(!handoff(dir.path()));
        clear_parked(dir.path());
        assert!(!parked(dir.path()));
    }

    #[test]
    fn a_cancelled_passcode_parks_and_hands_over() {
        let dir = tempfile::tempdir().unwrap();
        park(dir.path(), Reason::PasscodeCanceled);
        assert!(parked(dir.path()));
        assert!(handoff(dir.path()));
        set_handoff(dir.path(), false);
        assert!(!handoff(dir.path()));
    }
}
