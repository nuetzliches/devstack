//! The desktop's leave for a window to come to the front. A Wayland desktop
//! (GNOME, KDE) raises a window another program opens only with a token of
//! the click that asked for it; without one GNOME says "… is ready" instead.
//! The tray's host hands the token over right before it sends the click
//! (`ProvideXdgActivationToken`); the launch the click causes takes it and
//! gives it to the program it starts, in `XDG_ACTIVATION_TOKEN` (Wayland)
//! and `DESKTOP_STARTUP_ID` (X11; GNOME's token is good for both).
//!
//! A token is for one launch, right after its click: the first launch takes
//! it, and one older than [`FRESH`] is dropped. Where nothing provides one
//! (not the tray, not Linux) a launch keeps what this process inherited.

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long after its click a token is still taken.
pub const FRESH: Duration = Duration::from_secs(10);

/// The latest token and when it came.
#[derive(Debug, Default)]
struct Slot(Option<(String, Instant)>);

impl Slot {
    fn provide(&mut self, token: String, at: Instant) {
        self.0 = (!token.is_empty()).then_some((token, at));
    }

    /// The token, once, if it came no longer than [`FRESH`] before `now`.
    fn take(&mut self, now: Instant) -> Option<String> {
        self.0
            .take()
            .filter(|(_, at)| now.saturating_duration_since(*at) <= FRESH)
            .map(|(token, _)| token)
    }
}

static SLOT: Mutex<Slot> = Mutex::new(Slot(None));

/// Keep `token`, the leave of the click about to come, for its launch.
pub fn provide(token: String) {
    SLOT.lock().unwrap().provide(token, Instant::now());
}

/// Give `cmd` the token of the click that caused it, if one is fresh.
pub fn apply(cmd: &mut Command) {
    if let Some(token) = SLOT.lock().unwrap().take(Instant::now()) {
        cmd.env("XDG_ACTIVATION_TOKEN", &token)
            .env("DESKTOP_STARTUP_ID", &token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_taken_once() {
        let mut slot = Slot::default();
        let at = Instant::now();
        assert_eq!(slot.take(at), None);
        slot.provide("t1".into(), at);
        assert_eq!(
            slot.take(at + Duration::from_secs(1)).as_deref(),
            Some("t1")
        );
        assert_eq!(slot.take(at + Duration::from_secs(1)), None);
    }

    #[test]
    fn the_latest_token_counts_and_a_stale_one_is_dropped() {
        let mut slot = Slot::default();
        let at = Instant::now();
        slot.provide("t1".into(), at);
        slot.provide("t2".into(), at + Duration::from_secs(2));
        assert_eq!(
            slot.take(at + Duration::from_secs(3)).as_deref(),
            Some("t2")
        );

        slot.provide("t3".into(), at);
        assert_eq!(slot.take(at + FRESH + Duration::from_millis(1)), None);
        // Dropped, not kept for a later launch.
        assert_eq!(slot.take(at), None);

        slot.provide("t4".into(), at);
        slot.provide(String::new(), at);
        assert_eq!(slot.take(at), None);
    }

    #[test]
    fn a_launch_gets_the_token_in_both_variables() {
        provide("t5".into());
        let mut cmd = Command::new("true");
        apply(&mut cmd);
        let envs: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
            .collect();
        for name in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
            assert!(
                envs.contains(&(name.into(), Some("t5".into()))),
                "{name} in {envs:?}"
            );
        }
        let mut next = Command::new("true");
        apply(&mut next);
        assert_eq!(next.get_envs().count(), 0, "taken by the first launch");
    }
}
