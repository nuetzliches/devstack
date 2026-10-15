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

pub fn ui(print_only: bool) -> Result<()> {
    let url = pairing_url()?;
    if print_only {
        println!("{url}");
        return Ok(());
    }
    open_browser(&url)?;
    println!("dashboard opened (the link works once, for 60 s): {url}");
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
