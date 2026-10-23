//! `devstack hub`, `devstack ui`: the hub that serves the dashboard (ADR-0021).

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;
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
    if let Some(trouble) = v["trouble"].as_str() {
        println!("! {trouble}");
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
/// (ADR-0035), a connected one's services with their ports here (ADR-0053).
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
        let services: Vec<String> = l["services"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let name = s["name"].as_str()?;
                Some(match s["forwarded"].as_u64() {
                    Some(port) => format!("{name} (localhost:{port})"),
                    None => name.to_string(),
                })
            })
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

/// The lane called `lane` (or whose lease starts so) shared with this
/// machine, its lease, and the hub's query for `pool`.
fn shared_lane(lane: &str, pool: Option<&str>) -> Result<(Value, String, String)> {
    let q = pool_query(pool)?;
    let v = hub_get(&format!("/hub/pool/shared{q}"))?;
    let lanes = v.as_array().cloned().unwrap_or_default();
    let found: Vec<&Value> = lanes
        .iter()
        .filter(|l| l["lane"] == lane || l["lease"].as_str().is_some_and(|x| x.starts_with(lane)))
        .collect();
    let l = match found.as_slice() {
        [one] => (*one).clone(),
        [] => bail!(
            "no lane `{lane}` is shared with this machine (`devstack pool shared` lists them)"
        ),
        _ => bail!(
            "several shared lanes are called `{lane}`: name its lease (`devstack pool shared`)"
        ),
    };
    let lease = l["lease"].as_str().unwrap_or("").to_string();
    if lease.is_empty() || !lease.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("the hub names no usable lease for `{lane}`");
    }
    Ok((l, lease, q))
}

/// `devstack pool connect <lane>`: forward every service of a lane someone
/// shares with this machine here (ADR-0053).
pub fn connect_shared(lane: &str, pool: Option<&str>) -> Result<()> {
    let (l, lease, q) = shared_lane(lane, pool)?;
    let a = hub_send(
        "POST",
        &format!("/hub/pool/shared/{lease}/connect{q}"),
        None,
    )?;
    println!(
        "lane `{lane}` ({}, on {}) connected:",
        l["owner_name"].as_str().unwrap_or(""),
        l["node"]["name"].as_str().unwrap_or("")
    );
    for s in a["services"].as_array().into_iter().flatten() {
        println!(
            "  {:<16} {}",
            s["name"].as_str().unwrap_or(""),
            s["url"].as_str().unwrap_or("")
        );
    }
    if let Some(w) = a["warning"].as_str() {
        eprintln!("warning: {w}");
    }
    Ok(())
}

/// `devstack pool disconnect <lane>`: close the forwards to a shared lane,
/// its ports here are free again (ADR-0053).
pub fn disconnect_shared(lane: &str, pool: Option<&str>) -> Result<()> {
    let (_, lease, q) = shared_lane(lane, pool)?;
    let a = hub_send(
        "POST",
        &format!("/hub/pool/shared/{lease}/disconnect{q}"),
        None,
    )?;
    match a["closed"].as_u64().unwrap_or(0) {
        0 => println!("lane `{lane}` was not connected"),
        n => println!("lane `{lane}` disconnected: {n} port(s) here closed"),
    }
    Ok(())
}

/// `devstack open <lane> [service]`: open a service of a lane someone shares
/// with this machine, connecting the lane if it is not (ADR-0035 point 3,
/// ADR-0053 point 2).
pub fn open_shared(lane: &str, service: Option<&str>, pool: Option<&str>) -> Result<()> {
    let (l, lease, q) = shared_lane(lane, pool)?;
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
        "{service} of lane `{lane}` ({}, on {}): {url} (the lane is connected; `devstack pool disconnect {lane}` frees its ports)",
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

/// Where the dashboard shows a stack, on a lane or its main working tree.
pub struct DashboardLinks {
    /// The hash route: `/#/s/<id>?lane=<lane>`.
    pub route: String,
    /// The page, for a browser paired before.
    pub page: String,
    /// `devstack://open?stack=<id>&lane=<lane>`: pairs the browser that
    /// opens it first, then shows the page (ADR-0045 point 6).
    pub link: String,
}

/// The dashboard's links for the stack whose daemon runs from `main_file`
/// (in a lane, the main working tree's stack file); `lane: None` is the
/// main working tree. The id is the one the daemon registers.
pub fn dashboard_links(main_file: &Path, lane: Option<&str>) -> DashboardLinks {
    let abs = std::path::absolute(main_file).unwrap_or_else(|_| main_file.to_path_buf());
    let id = devstack_daemon::registry::id_for(&devstack_proc::long_path(&abs));
    let lane = crate::client::enc(lane.unwrap_or(""));
    let route = format!("/#/s/{id}?lane={lane}");
    DashboardLinks {
        page: format!("http://127.0.0.1:{}{route}", devstack_hub::port()),
        link: if lane.is_empty() {
            format!("devstack://open?stack={id}")
        } else {
            format!("devstack://open?stack={id}&lane={lane}")
        },
        route,
    }
}

/// `dashboard: <page>`: the line `status`, `up` and `lane start` end with.
pub fn dashboard_line(main_file: &Path, lane: Option<&str>) -> String {
    format!("dashboard: {}", dashboard_links(main_file, lane).page)
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

/// `devstack ui`: the dashboard in the browser, paired, on the stack of
/// `stack_file` when there is one, on `lane` or this worktree's lane. `link`
/// prints the lasting links instead (an agent hands them on), `print_only`
/// the one-time pairing link.
pub fn ui(
    stack_file: &Path,
    lane: Option<&str>,
    link: bool,
    print_only: bool,
    browser: Option<&str>,
) -> Result<()> {
    let here = stack_file.is_file().then(|| {
        let lane = lane
            .map(str::to_string)
            .or_else(|| crate::client::worktree_lane(stack_file));
        dashboard_links(&crate::client::daemon_file(stack_file), lane.as_deref())
    });
    if here.is_none() && (link || lane.is_some()) {
        bail!(
            "no stack file at {}: run it in the repository, or name the file with -f",
            stack_file.display()
        );
    }
    if link {
        let l = here.expect("checked above");
        println!("{}", l.page);
        println!("{}", l.link);
        return Ok(());
    }
    let mut url = pairing_url()?;
    if let Some(l) = &here {
        url = format!("{url}&next={}", devstack_hub::next_param(&l.route));
    }
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

/// Open `url` in the browser `name` (`--browser`, `DEVSTACK_BROWSER`,
/// `BROWSER`): a path as it is, a known name as that browser on this
/// platform, anything else as a program on PATH.
fn open_in(name: &str, url: &str) -> Result<()> {
    web_url(url)?;
    let program = devstack_desktop::browser_program(name);
    let started = devstack_desktop::start_browser(&program, url)
        .map_err(|e| anyhow!("cannot start {program} ({e}); open {url} yourself"))?;
    if !started {
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
    devstack_desktop::open_browser(url)
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

/// `devstack hub tray`: whether the tray runs, and why not.
pub fn tray_text() -> String {
    use devstack_hub::tray;
    match (tray::running(), tray::why_not()) {
        (Some(pid), _) => format!("tray running (pid {pid})"),
        (None, Some(why)) => format!("tray not running: {why}"),
        (None, None) => {
            "tray not running; it starts with the hub (`devstack hub tray on` starts it now)".into()
        }
    }
}

/// `devstack hub tray on`: with the hub from now on, and now.
pub fn tray_on() -> Result<()> {
    use devstack_hub::tray;
    tray::set_off(false).map_err(|e| anyhow!(e))?;
    if tray::running().is_none() {
        if let Some(why) = tray::why_not() {
            bail!("the tray cannot start: {why}");
        }
        tray::spawn().map_err(|e| anyhow!(e))?;
    }
    println!("tray on: it starts with the hub");
    Ok(())
}

/// `devstack hub tray off`: ended now, and not started with the hub.
pub fn tray_off() -> Result<()> {
    use devstack_hub::tray;
    tray::set_off(true).map_err(|e| anyhow!(e))?;
    if !tray::stop(Duration::from_secs(5)) {
        bail!("the tray did not end; it ends within a few seconds, or end it yourself");
    }
    println!("tray off: `devstack hub tray on` brings it back");
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
    fn the_dashboard_links_name_the_stack_the_daemon_registers() {
        let file = std::env::temp_dir().join("shop").join("devstack.toml");
        let id = devstack_daemon::registry::id_for(&devstack_proc::long_path(&file));
        let port = devstack_hub::port();
        let main = super::dashboard_links(&file, None);
        assert_eq!(main.route, format!("/#/s/{id}?lane="));
        assert_eq!(main.page, format!("http://127.0.0.1:{port}/#/s/{id}?lane="));
        assert_eq!(main.link, format!("devstack://open?stack={id}"));
        let lane = super::dashboard_links(&file, Some("feat-x"));
        assert_eq!(
            lane.page,
            format!("http://127.0.0.1:{port}/#/s/{id}?lane=feat-x")
        );
        assert_eq!(lane.link, format!("devstack://open?stack={id}&lane=feat-x"));
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
