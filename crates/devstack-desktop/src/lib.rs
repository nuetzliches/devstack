//! The desktop devstack runs on, one module per operating system (ADR-0054
//! point 6): what devstack registers in the user's session (the hub at logon,
//! the `devstack://` link), how the hub reaches the person at the machine
//! (desktop notifications, the folder dialog, the foreground), what it opens
//! (a browser, a folder in the user's programs, ADR-0046), starting the Docker
//! engine, and how this copy of devstack was installed (`devstack update`,
//! `devstack self uninstall`).
//!
//! `windows`, `linux` and `macos` each implement the same functions; this file
//! is the one place that names them, and what is the same everywhere. An
//! operating system without a module does not build, rather than compiling to
//! silent `None`s and `else` branches.

use std::path::{Path, PathBuf};
use std::process::Command;

pub mod activation;
pub mod channel;
// The macOS module's text, built and tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod mac;
pub mod notify;
pub mod tray;
pub mod webapp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as os;
#[cfg(target_os = "macos")]
use self::macos as os;
#[cfg(windows)]
use self::windows as os;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!(
    "devstack-desktop has no module for this operating system: add one beside windows.rs, linux.rs and macos.rs (ADR-0054)"
);

pub use channel::{
    homebrew_prefix, install_dir, is_cargo_install, is_dev_build, is_winget_install, stable_exe,
    undeleted,
};

// ------------------------------------------------------------- processes

/// A console program started from the hub or the CLI opens no window of its
/// own (Windows: `CREATE_NO_WINDOW`; elsewhere there is none to open).
pub fn no_window(cmd: &mut Command) {
    os::no_window(cmd);
}

/// The hub's own window in the foreground, until dropped (Windows: a
/// background process may neither take the foreground nor give it, so a
/// dialog or a program it starts would open behind the browser that asked).
/// `None` where that is not needed, or no window could be made.
pub use os::Front;

// ------------------------------------------------- the hub in the session

/// Start the hub at logon: `<exe> hub start`.
pub fn install_autostart(exe: &Path) -> Result<(), String> {
    os::install_autostart(exe)
}

pub fn remove_autostart() -> Result<(), String> {
    os::remove_autostart()
}

pub fn has_autostart() -> bool {
    os::has_autostart()
}

/// Register the `devstack://` link: the system runs `<exe> open-url <link>`.
pub fn install_link(exe: &Path) -> Result<(), String> {
    os::install_link(exe)
}

pub fn remove_link() -> Result<(), String> {
    os::remove_link()
}

pub fn has_link() -> bool {
    os::has_link()
}

// --------------------------------------------------------- the folder dialog

/// The operating system's folder dialog: the chosen folder, `None` when
/// cancelled, `Err` when no dialog can be shown (no desktop session). Called
/// from a worker thread of the hub.
pub fn pick_folder() -> Result<Option<String>, String> {
    os::pick_folder()
}

// ----------------------------------------------------------------- browsers

/// What opens a web address in the default browser on this system, for a
/// message that it did not start.
pub const URL_OPENER: &str = os::URL_OPENER;

/// Open `url` (checked to be a web address by the caller) in the default
/// browser, in the background.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    os::open_browser(url)
}

/// The dashboard's web app, installed from the hub on `origin`
/// (`http://127.0.0.1:4250`) in a Chromium-family browser, if this
/// platform's module finds one (ADR-0061).
pub fn dashboard_app(origin: &str) -> Option<webapp::WebApp> {
    os::dashboard_app(&webapp::app_id(&webapp::manifest_id(origin)))
}

/// Open `url` (a page of the hub on its origin) in the dashboard's installed
/// app when there is one, else in the default browser. `Ok(true)`: in the
/// app.
pub fn open_dashboard_page(url: &str) -> std::io::Result<bool> {
    if let Some(app) = webapp::origin_of(url).and_then(dashboard_app)
        && app.open(url).is_ok()
    {
        return Ok(true);
    }
    os::open_browser(url).map(|()| false)
}

/// Browsers known by name: the names one may give, and the program (Windows,
/// Linux) or application (macOS, `open -a`) each is on the platform.
const BROWSERS: &[(&[&str], [&str; 3])] = &[
    (
        &["chrome", "google-chrome"],
        ["chrome", "google-chrome", "Google Chrome"],
    ),
    (
        &["edge", "msedge", "microsoft-edge"],
        ["msedge", "microsoft-edge", "Microsoft Edge"],
    ),
    (&["firefox"], ["firefox", "firefox", "Firefox"]),
    (&["brave"], ["brave", "brave-browser", "Brave Browser"]),
    (&["chromium"], ["chromium", "chromium", "Chromium"]),
    (&["safari"], ["safari", "safari", "Safari"]),
];

/// The column of [`BROWSERS`] for each platform; a build uses its own one,
/// the tests all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Platform {
    Windows,
    Linux,
    Macos,
}

/// What a browser's name (`--browser`, `DEVSTACK_BROWSER`, `BROWSER`) is to
/// start on `platform`: a path as it is, a known name as that browser,
/// anything else as it is.
fn browser_for(name: &str, platform: Platform) -> String {
    let n = name.trim();
    if n.contains(['/', '\\']) {
        return n.to_string();
    }
    let lower = n.to_ascii_lowercase();
    BROWSERS
        .iter()
        .find(|(names, _)| names.contains(&lower.as_str()))
        .map_or(n, |(_, on)| on[platform as usize])
        .to_string()
}

/// What `--browser <name>` starts on this platform: a path as it is, a known
/// name as that browser's program (or application), anything else as a
/// program on `PATH`.
pub fn browser_program(name: &str) -> String {
    browser_for(name, os::PLATFORM)
}

/// Start `program` (from [`browser_program`]) on `url`. `Ok(false)`: the
/// system said it could not start it (where it says so).
pub fn start_browser(program: &str, url: &str) -> std::io::Result<bool> {
    os::start_browser(program, url)
}

// ------------------------------------------- the user's programs (ADR-0046)

/// What a folder viewer or terminal of this desktop is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    Files,
    Terminal,
}

/// The file manager or a terminal of this desktop, and how it opens a folder.
#[derive(Debug, Clone)]
pub struct Local {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: LocalKind,
    launch: os::Launch,
}

impl Local {
    /// Open the folder `dir` (`path`: it as the programs take it, without a
    /// verbatim prefix).
    pub fn open(&self, dir: &Path, path: &str) -> Result<(), String> {
        os::launch(&self.launch, dir, path)
    }
}

/// The file manager and the terminals of this desktop, in menu order;
/// nothing without a desktop (a server, an SSH session).
pub fn files_and_terminals() -> Vec<Local> {
    os::files_and_terminals()
}

/// Those of `schemes` the system opens with a program (an editor's
/// `vscode`, an agent's `claude`).
pub fn registered_schemes<'a>(schemes: &[&'a str]) -> Vec<&'a str> {
    os::registered_schemes(schemes)
}

/// What opening `target` in the system's shell does: a folder in the file
/// manager, a link in the program its scheme names.
pub fn shell_open(target: &str) -> Result<(), String> {
    os::shell_open(target)
}

/// GitHub Desktop, where it is installed: what starts it.
pub fn github_desktop() -> Option<PathBuf> {
    os::github_desktop()
}

/// The repository at `path` in GitHub Desktop (`exe` from
/// [`github_desktop`]), added to its list if it is not there yet.
pub fn github_desktop_open(exe: &Path, path: &str) -> Result<(), String> {
    os::github_desktop_open(exe, path)
}

/// Orca's CLI: on `PATH`, or in its install folder.
pub fn orca() -> Option<PathBuf> {
    os::orca()
}

/// Where `name` is on `PATH`, as a program (`.exe` added on Windows).
pub fn on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&file))
        // An app execution alias (`wt.exe`) is a reparse point that
        // `metadata` cannot follow; it is there all the same.
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir()))
}

// -------------------------------------------------------------------- Docker

/// What devstack can start the Docker engine with here (`Docker Desktop`),
/// if anything.
pub fn docker_starter() -> Option<&'static str> {
    os::docker_starter()
}

/// Start the engine [`docker_starter`] names.
pub fn start_docker() -> std::io::Result<()> {
    os::start_docker()
}

/// What to tell the user when devstack cannot start the engine.
pub fn docker_hint() -> &'static str {
    os::docker_hint()
}

/// What to tell the user when there is no `docker` at all.
pub fn docker_install_hint() -> &'static str {
    os::docker_install_hint()
}

// ----------------------------------------------------------- what is not there

/// A platform devstack runs on as a preview (ADR-0054): its name, and what
/// devstack does not do there yet, which `devstack doctor` says.
#[derive(Debug, Clone, Copy)]
pub struct Preview {
    pub name: &'static str,
    pub not_yet: &'static [&'static str],
}

/// `None` where devstack does everything.
pub const PREVIEW: Option<Preview> = os::PREVIEW;

/// Why there is no `devstack://` link here (macOS: "not on macOS yet"), so
/// the dashboard cannot start a stopped hub; `None` where there is one.
pub const LINK_UNAVAILABLE: Option<&str> = os::LINK_UNAVAILABLE;

/// More about the hub's start at logon than on or off, where there is more
/// to say (macOS: the LaunchAgent, and whether launchd has it loaded).
pub fn autostart_note() -> Option<String> {
    os::autostart_note()
}

// ------------------------------------------------------ self uninstall

/// The devstack processes of this user (pids), this one included.
pub fn devstack_pids() -> Vec<u32> {
    os::devstack_pids()
}

/// Take `dir` off the user's `PATH`, where the installer put it there.
pub fn remove_from_path(dir: &Path) -> Result<(), String> {
    os::remove_from_path(dir)
}

/// Delete the installed binary `exe` (on Windows its directory, once this
/// process has exited: see [`BINARY_GOES_LATER`]).
pub fn remove_binary(exe: &Path) -> Result<(), String> {
    os::remove_binary(exe)
}

/// The running binary cannot be deleted at once (Windows), so
/// [`remove_binary`] leaves it to a helper that waits for this process.
pub const BINARY_GOES_LATER: bool = os::BINARY_GOES_LATER;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsers_by_name_or_path() {
        use Platform::*;
        let b = browser_for;
        assert_eq!(b("Edge", Windows), "msedge");
        assert_eq!(b("Edge", Linux), "microsoft-edge");
        assert_eq!(b("chrome", Windows), "chrome");
        assert_eq!(b("chrome", Linux), "google-chrome");
        assert_eq!(b("firefox", Linux), "firefox");
        assert_eq!(b("brave", Linux), "brave-browser");
        assert_eq!(b(r"C:\Tools\vivaldi.exe", Windows), r"C:\Tools\vivaldi.exe");
        assert_eq!(b("/opt/x/browser", Linux), "/opt/x/browser");
        assert_eq!(b("vivaldi", Linux), "vivaldi");
        assert_eq!(b("Edge", Macos), "Microsoft Edge");
        assert_eq!(b("google-chrome", Macos), "Google Chrome");
        assert_eq!(b("firefox", Macos), "Firefox");
        assert_eq!(b("brave", Macos), "Brave Browser");
        assert_eq!(b("Safari", Macos), "Safari");
        assert_eq!(b("Arc", Macos), "Arc");
        assert_eq!(b("/Applications/Arc.app", Macos), "/Applications/Arc.app");
        assert_eq!(
            browser_program("chrome"),
            browser_for("chrome", os::PLATFORM)
        );
    }
}
