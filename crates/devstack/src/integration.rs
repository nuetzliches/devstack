//! The hub in the user's session (ADR-0021): it starts at logon, and a
//! `devstack://hub/start` link starts it from the dashboard, which cannot reach
//! a hub that does not run (nothing listens on its port).
//!
//! Both live in the user's profile, no admin rights (`devstack_desktop`):
//!
//! | | Windows | Linux | macOS |
//! | --- | --- | --- | --- |
//! | autostart | `HKCU\...\CurrentVersion\Run`, value `devstack-hub` | `~/.config/autostart/devstack-hub.desktop` | `~/Library/LaunchAgents/it.nuetzliche.devstack.hub.plist` (`launchctl bootstrap gui/<uid>`) |
//! | link | `HKCU\Software\Classes\devstack` | `~/.local/share/applications/devstack-url.desktop` + `xdg-mime` | not yet: it needs an app bundle (ADR-0054) |
//!
//! On Windows both run through `conhost.exe --headless`: devstack is a console
//! program, and started from the logon or from a browser it would otherwise
//! open a terminal window. Both name the binary by the path that stays: for a
//! copy Homebrew installed `<prefix>/bin/devstack`, not the versioned one
//! under `Cellar/`, which `brew cleanup` removes.
//!
//! `devstack up` installs both once, unless the user removed them
//! (`devstack hub uninstall` leaves a marker that says so) or opted out
//! (`DEVSTACK_NO_HUB_INTEGRATION`; the installers turn that into the marker).

use anyhow::{Result, anyhow};
use std::path::PathBuf;

/// What is registered right now.
pub struct Status {
    pub link: bool,
    pub autostart: bool,
}

/// What a `devstack://` link may do. A web page can open such a link too;
/// both only start the hub or show the dashboard, and the browser asks first.
const LINK_START: &str = "hub/start";
/// `devstack://open?stack=<id>&lane=<name>&service=<name>`: the dashboard
/// there, what a click on a notification opens (ADR-0045).
const LINK_OPEN: &str = "open";

fn marker() -> PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .join("hub")
        .join("integration.json")
}

fn write_marker(installed: bool) {
    let path = marker();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        serde_json::json!({ "installed": installed }).to_string(),
    );
}

fn exe() -> Result<PathBuf> {
    std::env::current_exe()
        .map(|e| devstack_desktop::stable_exe(&e))
        .map_err(|e| anyhow!("own path: {e}"))
}

/// Register link and (if asked) autostart for the running devstack binary.
pub fn install(autostart: bool) -> Result<Status> {
    let exe = exe()?;
    devstack_desktop::install_link(&exe).map_err(|e| anyhow!(e))?;
    if autostart {
        devstack_desktop::install_autostart(&exe).map_err(|e| anyhow!(e))?;
    } else {
        devstack_desktop::remove_autostart().map_err(|e| anyhow!(e))?;
    }
    write_marker(true);
    Ok(status())
}

pub fn uninstall() -> Result<()> {
    devstack_desktop::remove_link().map_err(|e| anyhow!(e))?;
    devstack_desktop::remove_autostart().map_err(|e| anyhow!(e))?;
    write_marker(false);
    Ok(())
}

pub fn status() -> Status {
    Status {
        link: devstack_desktop::has_link(),
        autostart: devstack_desktop::has_autostart(),
    }
}

/// The user's opt-out (`DEVSTACK_NO_HUB_INTEGRATION`, also what the
/// installers take): set and not `0`.
pub fn opted_out() -> bool {
    std::env::var_os("DEVSTACK_NO_HUB_INTEGRATION").is_some_and(|v| !v.is_empty() && v != "0")
}

/// From `devstack up`: install once, quietly. Not for a cargo build, not
/// while `DEVSTACK_NO_HUB_INTEGRATION` is set, and not again after the user
/// uninstalled it (the installers' opt-out runs `devstack hub uninstall`,
/// which leaves that marker).
pub fn ensure_once() {
    if opted_out() || marker().exists() {
        return;
    }
    let Ok(exe) = exe() else { return };
    if devstack_desktop::is_dev_build(&exe) {
        return;
    }
    match install(true) {
        Ok(_) if devstack_desktop::LINK_UNAVAILABLE.is_some() => eprintln!(
            "note: the dashboard hub now starts at logon (`devstack hub uninstall` undoes it)"
        ),
        Ok(_) => eprintln!(
            "note: the dashboard hub now starts at logon, and the dashboard can start it (`devstack hub uninstall` undoes both)"
        ),
        Err(e) => eprintln!("note: could not register the hub with your session: {e}"),
    }
}

/// `devstack open-url <url>`, what the system runs for a `devstack://` link.
pub fn open_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("devstack://")
        .or_else(|| url.strip_prefix("devstack:"))
        .ok_or_else(|| anyhow!("not a devstack link: {url}"))?;
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    match action.trim_matches('/') {
        LINK_START if query.is_empty() => {
            devstack_hub::ensure_running().map_err(|e| anyhow!(e))?;
            Ok(())
        }
        LINK_OPEN => crate::hubcmd::open_dashboard(&dashboard_route(query)?),
        other => Err(anyhow!("unknown devstack link `{other}`")),
    }
}

/// The dashboard route a `devstack://open` link names, from its query; each
/// value must be a name (a stack's id, a lane's, a service's), so nothing
/// else can be smuggled into the route.
fn dashboard_route(query: &str) -> Result<String> {
    let (mut stack, mut lane, mut service, mut page) = (None, None, None, None);
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v).ok_or_else(|| anyhow!("bad value in devstack link"))?;
        let is_name = !v.is_empty()
            && v.len() <= 100
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !is_name {
            return Err(anyhow!("`{v}` is no name (devstack link)"));
        }
        match k {
            "stack" => stack = Some(v),
            "lane" => lane = Some(v),
            "service" => service = Some(v),
            "page" => page = Some(v),
            _ => return Err(anyhow!("unknown `{k}` in devstack link")),
        }
    }
    match page.as_deref() {
        None => {}
        // Guest lanes on this machine (a notification's).
        Some("pool") if stack.is_none() => return Ok("/#/pool".into()),
        Some(p) => return Err(anyhow!("unknown page `{p}` in devstack link")),
    }
    let Some(stack) = stack else {
        return Ok("/#/".into());
    };
    if !stack.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("`{stack}` is no stack id"));
    }
    // The lane view (ADR-0081): no lane named is the main working tree, the
    // lane `main`; no lane is ever named so (ADR-0080).
    let mut route = format!("/#/s/{stack}/l/{}", lane.as_deref().unwrap_or("main"));
    if let Some(s) = service {
        route.push_str(&format!("/services/{s}"));
    }
    Ok(route)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn status_text() -> String {
    let s = status();
    let yn = |b: bool| if b { "on" } else { "off" };
    let autostart = match devstack_desktop::autostart_note() {
        Some(note) if s.autostart => format!("on ({note})"),
        _ => yn(s.autostart).to_string(),
    };
    format!(
        "starts at logon: {autostart}; dashboard can start it (devstack:// link): {}",
        devstack_desktop::LINK_UNAVAILABLE.unwrap_or(yn(s.link))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_other_than_hub_start_and_open_are_refused() {
        assert!(open_url("https://example.com").is_err());
        assert!(open_url("devstack://run/reset-db").is_err());
        assert!(open_url("devstack://../../x").is_err());
        assert!(open_url("devstack://hub/start?x=1").is_err());
    }

    #[test]
    fn an_open_link_names_a_dashboard_route_and_nothing_else() {
        assert_eq!(
            dashboard_route("stack=ab12&lane=feat-x&service=api").unwrap(),
            "/#/s/ab12/l/feat-x/services/api"
        );
        assert_eq!(dashboard_route("stack=ab12").unwrap(), "/#/s/ab12/l/main");
        assert_eq!(dashboard_route("").unwrap(), "/#/");
        assert_eq!(dashboard_route("page=pool").unwrap(), "/#/pool");
        assert!(dashboard_route("page=settings").is_err());
        for bad in [
            "stack=shop",
            "stack=ab12&lane=..%2F..",
            "stack=ab12&service=a%3Fb",
            "stack=ab12&service=",
            "stack=ab12&next=http://evil",
            "stack=ab12&lane=%zz",
        ] {
            assert!(dashboard_route(bad).is_err(), "{bad}");
        }
    }
}
