//! Where the hub's desktop notifications go on this operating system
//! (ADR-0045): a WinRT toast on Windows, `org.freedesktop.Notifications` on
//! the session bus on Linux. What notifies, and when, is the hub's
//! (`devstack-hub`'s `notify`); this is only the sink.

use std::sync::Arc;

/// A notification as the desktop shows it.
#[derive(Debug, Clone)]
pub struct Note {
    pub title: String,
    /// Whose: the stack, and the lane.
    pub context: String,
    pub body: String,
    /// The dashboard's route a click shows (`/#/s/<id>?lane=`).
    pub route: String,
    /// The `devstack://open` link that shows it (`devstack open-url`).
    pub link: String,
    /// The same tag replaces an earlier notification of it where the
    /// desktop keeps them (Windows' notification centre).
    pub tag: String,
}

/// Where notifications go on this platform.
pub trait Sink: Send + Sync {
    /// `Err` with why nothing shows (no desktop session, switched off in
    /// the system's settings, not on this platform).
    fn status(&self) -> Result<(), String>;
    /// What does not work as it should while notifications show (devstack's
    /// name not registered with Windows, a click that could not open the
    /// dashboard), or `None`.
    fn trouble(&self) -> Option<String> {
        None
    }
    /// Show it, in the background; a failure goes to [`Hooks::failed`].
    fn show(&self, n: Note);
}

/// Nothing shows, and why.
pub struct Off(pub &'static str);

impl Sink for Off {
    fn status(&self) -> Result<(), String> {
        Err(self.0.into())
    }
    fn show(&self, _: Note) {}
}

/// Shows the dashboard at a route, for a click the hub hears itself
/// (Linux); `Err` with why it could not.
pub type Open = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// What went wrong in the background, for the hub's log and the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A notification could not be shown: the error.
    NotShown(String),
    /// What notifications need registered could not be (Windows' name for
    /// devstack): the error.
    NotRegistered(String),
}

/// What the sink calls back into the hub with.
#[derive(Clone)]
pub struct Hooks {
    pub open: Open,
    pub failed: Arc<dyn Fn(Failure) + Send + Sync>,
}

/// The sink of this platform, and on Windows the registration its toasts
/// need. `port` is the hub's (a Windows toast opens the dashboard there when
/// the `devstack://` link is not registered). Needs a tokio runtime.
pub fn sink(port: u16, hooks: Hooks) -> Box<dyn Sink> {
    crate::os::notify_sink(port, hooks)
}

/// Remove what the hub registered with the system for its notifications
/// (`devstack self uninstall`).
pub fn unregister() {
    crate::os::notify_unregister();
}
