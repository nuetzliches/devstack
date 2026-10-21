//! The hub in the user's session (ADR-0021): it starts at logon, and a
//! `devstack://hub/start` link starts it from the dashboard, which cannot reach
//! a hub that does not run (nothing listens on its port).
//!
//! Both live in the user's profile, no admin rights:
//!
//! | | Windows | Linux |
//! | --- | --- | --- |
//! | autostart | `HKCU\...\CurrentVersion\Run`, value `devstack-hub` | `~/.config/autostart/devstack-hub.desktop` |
//! | link | `HKCU\Software\Classes\devstack` | `~/.local/share/applications/devstack-url.desktop` + `xdg-mime` |
//!
//! On Windows both run through `conhost.exe --headless`: devstack is a console
//! program, and started from the logon or from a browser it would otherwise
//! open a terminal window.
//!
//! `devstack up` installs both once, unless the user removed them
//! (`devstack hub uninstall` leaves a marker that says so) or opted out
//! (`DEVSTACK_NO_HUB_INTEGRATION`; the installers turn that into the marker).

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

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

/// A cargo build (`.../target/...`) is not where the user keeps devstack; a
/// registration pointing there breaks with the next `cargo clean`.
pub(crate) fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "target")
}

fn exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("own path: {e}"))
}

/// Register link and (if asked) autostart for the running devstack binary.
pub fn install(autostart: bool) -> Result<Status> {
    let exe = exe()?;
    imp::install_link(&exe)?;
    if autostart {
        imp::install_autostart(&exe)?;
    } else {
        imp::remove_autostart()?;
    }
    write_marker(true);
    Ok(status())
}

pub fn uninstall() -> Result<()> {
    imp::remove_link()?;
    imp::remove_autostart()?;
    write_marker(false);
    Ok(())
}

pub fn status() -> Status {
    Status {
        link: imp::has_link(),
        autostart: imp::has_autostart(),
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
    if is_dev_build(&exe) {
        return;
    }
    match install(true) {
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
    let (mut stack, mut lane, mut service) = (None, None, None);
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
            _ => return Err(anyhow!("unknown `{k}` in devstack link")),
        }
    }
    let Some(stack) = stack else {
        return Ok("/#/".into());
    };
    if !stack.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("`{stack}` is no stack id"));
    }
    let mut route = format!("/#/s/{stack}");
    if let Some(s) = service {
        route.push_str(&format!("/services/{s}"));
    }
    route.push_str(&format!("?lane={}", lane.unwrap_or_default()));
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
    format!(
        "starts at logon: {}; dashboard can start it (devstack:// link): {}",
        yn(s.autostart),
        yn(s.link)
    )
}

/// `mimeapps.list` without devstack's `x-scheme-handler/devstack` entries
/// (another handler the user chose for the scheme stays).
#[cfg_attr(windows, allow(dead_code))]
fn without_handler(text: &str) -> String {
    let mut out: String = text
        .lines()
        .filter(|l| {
            let l = l.trim();
            !(l.starts_with("x-scheme-handler/devstack=") && l.contains("devstack-url.desktop"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::process::{Command, Stdio};

    const CLASSES: &str = r"HKCU\Software\Classes\devstack";
    const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "devstack-hub";

    fn reg(args: &[&str]) -> Result<bool> {
        use std::os::windows::process::CommandExt;
        let s = Command::new("reg")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()
            .map_err(|e| anyhow!("reg.exe: {e}"))?;
        Ok(s.success())
    }

    fn must(args: &[&str]) -> Result<()> {
        if reg(args)? {
            Ok(())
        } else {
            Err(anyhow!("`reg {}` failed", args.join(" ")))
        }
    }

    /// `"C:\Windows\System32\conhost.exe" --headless "<exe>" <args>`
    fn headless(exe: &Path, args: &str) -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        format!(
            r#""{root}\System32\conhost.exe" --headless "{}" {args}"#,
            exe.display()
        )
    }

    pub fn install_link(exe: &Path) -> Result<()> {
        must(&["add", CLASSES, "/ve", "/d", "URL:devstack", "/f"])?;
        must(&["add", CLASSES, "/v", "URL Protocol", "/d", "", "/f"])?;
        let cmd = headless(exe, r#"open-url "%1""#);
        must(&[
            "add",
            &format!(r"{CLASSES}\shell\open\command"),
            "/ve",
            "/d",
            &cmd,
            "/f",
        ])
    }

    pub fn remove_link() -> Result<()> {
        if has_link() {
            must(&["delete", CLASSES, "/f"])?;
        }
        Ok(())
    }

    pub fn has_link() -> bool {
        reg(&["query", &format!(r"{CLASSES}\shell\open\command")]).unwrap_or(false)
    }

    pub fn install_autostart(exe: &Path) -> Result<()> {
        must(&[
            "add",
            RUN,
            "/v",
            RUN_VALUE,
            "/d",
            &headless(exe, "hub start"),
            "/f",
        ])
    }

    pub fn remove_autostart() -> Result<()> {
        if has_autostart() {
            must(&["delete", RUN, "/v", RUN_VALUE, "/f"])?;
        }
        Ok(())
    }

    pub fn has_autostart() -> bool {
        reg(&["query", RUN, "/v", RUN_VALUE]).unwrap_or(false)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::process::{Command, Stdio};

    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    }

    fn data_dir() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
    }

    fn config_dir() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
    }

    fn link_file() -> PathBuf {
        data_dir().join("applications/devstack-url.desktop")
    }

    fn autostart_file() -> PathBuf {
        config_dir().join("autostart/devstack-hub.desktop")
    }

    fn write(path: &Path, text: &str) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text).map_err(|e| anyhow!("{}: {e}", path.display()))
    }

    pub fn install_link(exe: &Path) -> Result<()> {
        write(
            &link_file(),
            &format!(
                "[Desktop Entry]\nType=Application\nName=devstack\nExec=\"{}\" open-url %u\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/devstack;\n",
                exe.display()
            ),
        )?;
        // Without xdg-utils (a server, a container) the file is there but no
        // browser asks for it; that is all we can do.
        let _ = Command::new("xdg-mime")
            .args([
                "default",
                "devstack-url.desktop",
                "x-scheme-handler/devstack",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }

    pub fn remove_link() -> Result<()> {
        let _ = std::fs::remove_file(link_file());
        // `xdg-mime default` wrote the association into mimeapps.list; it
        // would point at a file that is gone.
        for list in [
            config_dir().join("mimeapps.list"),
            data_dir().join("applications/mimeapps.list"),
        ] {
            if let Ok(text) = std::fs::read_to_string(&list) {
                let kept = super::without_handler(&text);
                if kept != text {
                    let _ = std::fs::write(&list, kept);
                }
            }
        }
        Ok(())
    }

    pub fn has_link() -> bool {
        link_file().is_file()
    }

    pub fn install_autostart(exe: &Path) -> Result<()> {
        write(
            &autostart_file(),
            &format!(
                "[Desktop Entry]\nType=Application\nName=devstack hub\nComment=Serves the devstack dashboard\nExec=\"{}\" hub start\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
            ),
        )
    }

    pub fn remove_autostart() -> Result<()> {
        let _ = std::fs::remove_file(autostart_file());
        Ok(())
    }

    pub fn has_autostart() -> bool {
        autostart_file().is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cargo_build_is_recognised() {
        assert!(is_dev_build(Path::new(
            "/src/devstack/target/release/devstack"
        )));
        assert!(!is_dev_build(Path::new("/home/u/.local/bin/devstack")));
    }

    #[test]
    fn the_scheme_handler_leaves_mimeapps_list() {
        let text = "[Default Applications]\nx-scheme-handler/devstack=devstack-url.desktop\ntext/html=firefox.desktop\n[Added Associations]\nx-scheme-handler/devstack=devstack-url.desktop;\n";
        assert_eq!(
            without_handler(text),
            "[Default Applications]\ntext/html=firefox.desktop\n[Added Associations]\n"
        );
        let other = "x-scheme-handler/devstack=mine.desktop\n";
        assert_eq!(without_handler(other), other);
    }

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
            "/#/s/ab12/services/api?lane=feat-x"
        );
        assert_eq!(dashboard_route("stack=ab12").unwrap(), "/#/s/ab12?lane=");
        assert_eq!(dashboard_route("").unwrap(), "/#/");
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
