//! `devstack hub`, `devstack ui`: the hub that serves the dashboard (ADR-0021).

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn token() -> Result<String> {
    devstack_daemon::token::load_or_create().map_err(|e| anyhow!("token: {e}"))
}

/// Hub state as text (also the MCP tool `hub_status`).
pub fn status_text() -> String {
    format!(
        "{}
{}",
        running_text(),
        crate::integration::status_text()
    )
}

fn running_text() -> String {
    let port = devstack_hub::port();
    match devstack_hub::read_info().filter(|_| devstack_hub::answers(port)) {
        Some(i) => format!(
            "hub running: http://127.0.0.1:{} (pid {}, devstack {})",
            i.port, i.pid, i.version
        ),
        None if devstack_hub::answers(port) => {
            format!("port {port} answers, but not as a devstack hub of this user")
        }
        None => format!("hub not running (port {port}); `devstack ui` starts it"),
    }
}

/// Start the hub if needed and get a pairing link for the browser.
pub fn pairing_url() -> Result<String> {
    let port = devstack_hub::ensure_running().map_err(|e| anyhow!(e))?;
    let mut r = agent()
        .post(&format!("http://127.0.0.1:{port}/hub/pair"))
        .header("Authorization", &format!("Bearer {}", token()?))
        .send_empty()?;
    if r.status() != 200 {
        bail!("the hub refused pairing ({})", r.status());
    }
    let v: Value = serde_json::from_str(&r.body_mut().read_to_string()?)?;
    v["url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no url in the hub's answer"))
}

pub fn ui(print_only: bool, browser: Option<&str>) -> Result<()> {
    let url = pairing_url()?;
    if print_only {
        println!("{url}");
        return Ok(());
    }
    // The pairing belongs to the browser that opens the link, so it must be
    // the one you develop in, not just the system default (#9).
    let chosen = browser
        .map(str::to_string)
        .or_else(|| std::env::var("DEVSTACK_BROWSER").ok())
        .or_else(|| std::env::var("BROWSER").ok())
        .filter(|b| !b.trim().is_empty());
    match &chosen {
        Some(b) => open_in(b, &url)?,
        None => open_browser(&url)?,
    }
    println!(
        "dashboard opened{} (the link works once, for 60 s): {url}",
        chosen.map(|b| format!(" in {b}")).unwrap_or_default()
    );
    Ok(())
}

/// What `--browser` names, as a program to start: a path as it is, a known
/// name as that browser's executable on this platform, anything else as a
/// program on PATH.
fn browser_program(name: &str) -> String {
    let n = name.trim();
    if n.contains(['/', '\\']) {
        return n.to_string();
    }
    let known = |win: &'static str, unix: &'static str| if cfg!(windows) { win } else { unix };
    match n.to_ascii_lowercase().as_str() {
        "chrome" | "google-chrome" => known("chrome", "google-chrome"),
        "edge" | "msedge" | "microsoft-edge" => known("msedge", "microsoft-edge"),
        "firefox" => known("firefox", "firefox"),
        "brave" => known("brave", "brave-browser"),
        "chromium" => known("chromium", "chromium"),
        _ => n,
    }
    .to_string()
}

/// Open `url` in the browser `name` (`--browser`, `DEVSTACK_BROWSER`, `BROWSER`).
fn open_in(name: &str, url: &str) -> Result<()> {
    let program = browser_program(name);
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        // `start` finds browsers by their registered name (App Paths), which
        // are rarely on PATH; the empty title keeps a quoted path a program.
        let mut c = std::process::Command::new("cmd");
        c.args(["/d", "/c", "start", "", &program, url]);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new(&program);
        c.arg(url);
        c
    };
    let status = cmd
        .status()
        .map_err(|e| anyhow!("cannot start {program} ({e}); open {url} yourself"))?;
    if cfg!(windows) && !status.success() {
        bail!(
            "cannot start {program}; open {url} yourself, or pass the browser's path to --browser"
        );
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| anyhow!("cannot open a browser ({e}); open {url} yourself"))
}

pub fn stop() -> Result<()> {
    let port = devstack_hub::port();
    if !devstack_hub::answers(port) {
        println!("hub not running");
        return Ok(());
    }
    let r = agent()
        .post(&format!("http://127.0.0.1:{port}/hub/shutdown"))
        .header("Authorization", &format!("Bearer {}", token()?))
        .send_empty()?;
    if r.status().as_u16() >= 400 {
        bail!("the hub refused to stop ({})", r.status());
    }
    println!("hub stopped");
    Ok(())
}

/// Start the hub alongside a stack (`devstack up`), unless `DEVSTACK_NO_HUB`
/// is set. A failure is a note, never a reason to fail `up`.
pub fn ensure_quietly() {
    if std::env::var_os("DEVSTACK_NO_HUB").is_some() {
        return;
    }
    crate::integration::ensure_once();
    if let Err(e) = devstack_hub::ensure_running() {
        eprintln!("note: the dashboard hub did not start: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn browsers_by_name_or_path() {
        let b = super::browser_program;
        assert_eq!(
            b("Edge"),
            if cfg!(windows) {
                "msedge"
            } else {
                "microsoft-edge"
            }
        );
        assert_eq!(
            b("chrome"),
            if cfg!(windows) {
                "chrome"
            } else {
                "google-chrome"
            }
        );
        assert_eq!(b("firefox"), "firefox");
        assert_eq!(b(r"C:\Tools\vivaldi.exe"), r"C:\Tools\vivaldi.exe");
        assert_eq!(b("/opt/x/browser"), "/opt/x/browser");
        assert_eq!(b("vivaldi"), "vivaldi");
    }
}
