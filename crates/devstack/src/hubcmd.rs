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
    use devstack_hub::Listener;
    let port = devstack_hub::port();
    match (devstack_hub::who_answers(port), devstack_hub::read_info()) {
        (Listener::Ours, Some(i)) => format!(
            "hub running: http://127.0.0.1:{} (pid {}, devstack {})",
            i.port, i.pid, i.version
        ),
        (Listener::Ours, None) => format!("hub running: http://127.0.0.1:{port}"),
        (Listener::Foreign, _) => {
            format!("port {port} answers, but not as a devstack hub of this user")
        }
        (Listener::Nothing, _) => {
            format!("hub not running (port {port}); `devstack ui` starts it")
        }
    }
}

/// `GET` a hub path as this user (the hub is started if needed).
fn hub_get(path: &str) -> Result<Value> {
    let port = devstack_hub::ensure_running().map_err(|e| anyhow!(e))?;
    let mut r = agent()
        .get(&format!("http://127.0.0.1:{port}{path}"))
        .header("Authorization", &format!("Bearer {}", token()?))
        .call()?;
    let text = r.body_mut().read_to_string()?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if r.status() != 200 {
        bail!("{}", v["error"].as_str().unwrap_or(&text));
    }
    Ok(v)
}

/// `method path` on the hub with a JSON body; the answer, or its error.
fn hub_send(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let port = devstack_hub::ensure_running().map_err(|e| anyhow!(e))?;
    let url = format!("http://127.0.0.1:{port}{path}");
    let auth = format!("Bearer {}", token()?);
    let a = agent();
    let mut r = match (method, body) {
        ("PUT", Some(b)) => a
            .put(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .send(b.to_string())?,
        ("POST", _) => a.post(&url).header("Authorization", &auth).send_empty()?,
        _ => bail!("unsupported request {method} {path}"),
    };
    let text = r.body_mut().read_to_string()?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !r.status().is_success() {
        bail!("{}", v["error"].as_str().unwrap_or(&text));
    }
    Ok(v)
}

/// `devstack hub notify`: what notifies, and whether the desktop shows it.
pub fn notify_show() -> Result<()> {
    print_notify(&hub_get("/hub/notify")?);
    Ok(())
}

fn print_notify(v: &Value) {
    match v["unavailable"].as_str() {
        None => println!("notifications: shown by the desktop"),
        Some(why) => println!("notifications: not shown ({why})"),
    }
    for k in v["kinds"].as_array().into_iter().flatten() {
        println!(
            "  {:<3} {:<15} {}",
            if k["on"] == true { "on" } else { "off" },
            k["kind"].as_str().unwrap_or(""),
            k["label"].as_str().unwrap_or("")
        );
    }
    let muted: Vec<&str> = v["muted"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !muted.is_empty() {
        println!("muted (stack id, or stack id/lane): {}", muted.join(", "));
    }
}

/// `devstack hub notify on|off <kind>...|all`.
pub fn notify_set(kinds: &[String], on: bool) -> Result<()> {
    let names: Vec<String> = if kinds.iter().any(|k| k == "all") {
        hub_get("/hub/notify")?["kinds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k["kind"].as_str().map(str::to_string))
            .collect()
    } else {
        kinds.to_vec()
    };
    let map: serde_json::Map<String, Value> =
        names.into_iter().map(|k| (k, Value::Bool(on))).collect();
    let v = hub_send(
        "PUT",
        "/hub/notify",
        Some(serde_json::json!({ "kinds": map })),
    )?;
    print_notify(&v);
    Ok(())
}

/// `devstack hub notify test`.
pub fn notify_test() -> Result<()> {
    hub_send("POST", "/hub/notify/test", None)?;
    println!("a test notification is on its way; a click on it opens the dashboard");
    Ok(())
}

/// `?pool=<name>` for a pool other than the default one; a name that is no
/// pool's (ADR-0034) is refused before it goes into a URL.
fn pool_query(pool: Option<&str>) -> Result<String> {
    let Some(p) = pool.filter(|p| *p != devstack_node::member::DEFAULT_POOL) else {
        return Ok(String::new());
    };
    if !devstack_node::member::valid_pool_name(p) {
        bail!("--pool {p}: a-z, 0-9, `-` and `_`, at most 32");
    }
    Ok(format!("?pool={p}"))
}

/// `devstack pool shared`: the lanes others share with this machine
/// (ADR-0035).
pub fn shared(pool: Option<&str>) -> Result<()> {
    let v = hub_get(&format!("/hub/pool/shared{}", pool_query(pool)?))?;
    let lanes = v.as_array().cloned().unwrap_or_default();
    if lanes.is_empty() {
        println!("nobody shares a lane with this machine");
        return Ok(());
    }
    println!(
        "{:<24} {:<16} {:<18} {:<16} SERVICES",
        "LANE", "REPOSITORY", "ON", "SHARED BY"
    );
    for l in lanes {
        let services: Vec<&str> = l["services"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["name"].as_str())
            .collect();
        println!(
            "{:<24} {:<16} {:<18} {:<16} {}",
            l["lane"].as_str().unwrap_or(""),
            l["repo"].as_str().unwrap_or(""),
            l["node"]["name"].as_str().unwrap_or(""),
            l["owner_name"].as_str().unwrap_or(""),
            services.join(", ")
        );
    }
    Ok(())
}

/// `devstack open <lane> [service]`: open a service of a lane someone shares
/// with this machine, through a forward the hub keeps (ADR-0035 point 3).
pub fn open_shared(lane: &str, service: Option<&str>, pool: Option<&str>) -> Result<()> {
    let q = pool_query(pool)?;
    let v = hub_get(&format!("/hub/pool/shared{q}"))?;
    let lanes = v.as_array().cloned().unwrap_or_default();
    let found: Vec<&Value> = lanes
        .iter()
        .filter(|l| l["lane"] == lane || l["lease"].as_str().is_some_and(|x| x.starts_with(lane)))
        .collect();
    let l = match found.as_slice() {
        [one] => *one,
        [] => bail!(
            "no lane `{lane}` is shared with this machine (`devstack pool shared` lists them)"
        ),
        _ => bail!(
            "several shared lanes are called `{lane}`: name its lease (`devstack pool shared`)"
        ),
    };
    let services: Vec<&str> = l["services"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["name"].as_str())
        .collect();
    let service = match (service, services.as_slice()) {
        (Some(s), _) => s,
        (None, [one]) => one,
        (None, _) => bail!("which service: {}", services.join(", ")),
    };
    let port = devstack_hub::ensure_running().map_err(|e| anyhow!(e))?;
    let lease = l["lease"].as_str().unwrap_or("");
    if lease.is_empty() || !lease.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("the hub names no usable lease for `{lane}`");
    }
    let mut r = agent()
        .post(&format!(
            "http://127.0.0.1:{port}/hub/pool/shared/{lease}/open{q}"
        ))
        .header("Authorization", &format!("Bearer {}", token()?))
        .header("Content-Type", "application/json")
        .send(serde_json::json!({ "service": service }).to_string())?;
    let text = r.body_mut().read_to_string()?;
    let a: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let Some(url) = a["url"].as_str() else {
        bail!("{}", a["error"].as_str().unwrap_or(&text));
    };
    println!(
        "{service} of lane `{lane}` ({}, on {}): {url}",
        l["owner_name"].as_str().unwrap_or(""),
        l["node"]["name"].as_str().unwrap_or("")
    );
    if let Some(w) = a["warning"].as_str() {
        eprintln!("warning: {w}");
    }
    let _ = open_browser(url);
    Ok(())
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

/// Open the dashboard at `route` (a hash route, `/#/s/<id>?lane=`), paired
/// like `devstack ui`: a click on a notification (`devstack://open`).
pub fn open_dashboard(route: &str) -> Result<()> {
    let url = format!(
        "{}&next={}",
        pairing_url()?,
        devstack_hub::next_param(route)
    );
    match chosen_browser(None) {
        Some(b) => open_in(&b, &url),
        None => open_browser(&url),
    }
}

/// `devstack ui` and `devstack://open` name the browser the same way.
fn chosen_browser(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("DEVSTACK_BROWSER").ok())
        .or_else(|| std::env::var("BROWSER").ok())
        .filter(|b| !b.trim().is_empty())
}

pub fn ui(print_only: bool, browser: Option<&str>) -> Result<()> {
    let url = pairing_url()?;
    if print_only {
        println!("{url}");
        return Ok(());
    }
    // The pairing belongs to the browser that opens the link, so it must be
    // the one you develop in, not just the system default (#9).
    let chosen = chosen_browser(browser);
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
    web_url(url)?;
    let program = browser_program(name);
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        // `start` finds browsers by their registered name (App Paths), which
        // are rarely on PATH; the empty title keeps a quoted path a program.
        // cmd reads the line again: the address in quotes, or an `&` in it
        // (`/auth?code=…&next=…`) would end the command.
        let mut c = std::process::Command::new("cmd");
        c.args(["/d", "/c", "start", "", &program]);
        c.raw_arg(format!("\"{url}\""));
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

/// Only web pages: what a browser opens is handed to the system's protocol
/// handlers, and a scheme like `ms-msdt:` or `search-ms:` starts programs.
fn web_url(url: &str) -> Result<()> {
    let lower = url.trim_start().to_ascii_lowercase();
    if (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.contains(['"', '\n', '\r'])
    {
        Ok(())
    } else {
        bail!("not opening `{url}`: only http and https addresses are opened")
    }
}

fn open_browser(url: &str) -> Result<()> {
    web_url(url)?;
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
    use devstack_hub::Listener;
    let port = devstack_hub::port();
    match devstack_hub::who_answers(port) {
        Listener::Nothing => {
            println!("hub not running");
            return Ok(());
        }
        Listener::Foreign => {
            bail!("port {port} answers, but not as your devstack hub; not sending it your token")
        }
        Listener::Ours => {}
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

    #[test]
    fn only_web_addresses_are_opened() {
        let ok = |u: &str| super::web_url(u).is_ok();
        assert!(ok("http://localhost:4200"));
        assert!(ok("HTTPS://localhost:4200/x"));
        assert!(!ok("ms-msdt://localhost:1234"));
        assert!(!ok("search-ms:query=x"));
        assert!(!ok("file:///C:/Windows"));
        assert!(!ok("javascript:alert(1)"));
        // cmd gets the address in quotes (`open_in`): it may not end them.
        assert!(!ok("http://x/\" & calc & \""));
        assert!(ok("http://127.0.0.1:4250/auth?code=c&next=%2F%23%2F"));
    }
}
