//! `devstack tray` (ADR-0060): the tray icon, a client of the hub. The main
//! thread shows it (`devstack_desktop::tray`); a worker reads `/hub/summary`
//! every two seconds, makes the menu from it, and does what is chosen, each
//! action a request to the hub on a thread of its own. It works out no state
//! itself and never talks to a daemon.
//!
//! One per user: the lock beside the hub's state. The hub starts it; it
//! ends when chosen in its menu, when switched off (`devstack hub tray off`,
//! `tray.json`) or asked to (`tray.stop`), and hands over to the tray of a
//! newer devstack the hub runs.

use anyhow::{Result, anyhow, bail};
use devstack_desktop::tray::{Entry, Menu, QUIT, Tone, one_line};
use devstack_hub::{Listener, tray as hubtray};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(2);
/// How long an action's failure stays in the menu.
const SHOW_ERROR: Duration = Duration::from_secs(60);
/// Set for the tray a tray started for the hub's newer devstack, to that
/// version: if it is not that version either, it does not hand over again.
const HANDED_OVER: &str = "DEVSTACK_TRAY_HANDED_OVER";

/// What the hub says, or why it says nothing.
#[derive(Debug, Clone, PartialEq)]
enum Hub {
    Running(Value),
    /// The hub answers, but the summary could not be read.
    Unreadable(String),
    Stopped,
    /// Another program answers on the hub's port.
    Foreign,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn token() -> Result<String> {
    devstack_daemon::token::load()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("no token yet"))
}

fn fetch() -> Hub {
    let port = devstack_hub::port();
    match devstack_hub::who_answers(port) {
        Listener::Nothing => return Hub::Stopped,
        Listener::Foreign => return Hub::Foreign,
        Listener::Ours => {}
    }
    let read = || -> Result<Value> {
        let mut r = agent()
            .get(&format!("http://127.0.0.1:{port}/hub/summary"))
            .header("Authorization", &format!("Bearer {}", token()?))
            .call()?;
        let text = r.body_mut().read_to_string()?;
        if r.status() != 200 {
            bail!("the hub answered {}", r.status());
        }
        Ok(serde_json::from_str(&text)?)
    };
    match read() {
        Ok(v) => Hub::Running(v),
        Err(e) => Hub::Unreadable(e.to_string()),
    }
}

/// `POST` to the hub, as the dashboard does.
fn post(path: &str, body: Option<Value>) -> Result<()> {
    let port = devstack_hub::port();
    if devstack_hub::who_answers(port) != Listener::Ours {
        bail!("the hub does not run");
    }
    let req = agent()
        .post(&format!("http://127.0.0.1:{port}{path}"))
        .header("Authorization", &format!("Bearer {}", token()?))
        .header("X-Devstack-Client", "tray");
    let mut r = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .send(b.to_string())?,
        None => req.send_empty()?,
    };
    if !r.status().is_success() {
        let text = r.body_mut().read_to_string().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        bail!("{}", v["error"].as_str().unwrap_or(&text));
    }
    Ok(())
}

fn action(label: &str, id: String, enabled: bool) -> Entry {
    Entry::Action {
        label: label.into(),
        id,
        enabled,
    }
}

fn plural(n: u64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// `/#/s/<id>/l/<lane>` (`main`: the main working tree, ADR-0081), and
/// with a service its page.
fn route(stack: &str, lane: Option<&str>, service: Option<&str>) -> String {
    let enc = crate::client::enc;
    let lane = lane.filter(|l| !l.is_empty()).unwrap_or("main");
    let mut r = format!("/#/s/{}/l/{}", enc(stack), enc(lane));
    if let Some(s) = service {
        r.push_str(&format!("/services/{}", enc(s)));
    }
    r
}

/// The tray's menu for what the hub said, and an action's failure.
fn menu(hub: &Hub, error: Option<&str>) -> Menu {
    let mut entries = Vec::new();
    let (tone, head) = match hub {
        Hub::Stopped => (Tone::Idle, "The dashboard hub does not run".to_string()),
        Hub::Foreign => (
            Tone::Attention,
            format!("Port {} is taken by another program", devstack_hub::port()),
        ),
        Hub::Unreadable(e) => (Tone::Attention, format!("The hub does not answer: {e}")),
        Hub::Running(v) => {
            let stacks = v["stacks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let running = stacks.iter().filter(|s| s["running"] == true).count() as u64;
            let attention: u64 = stacks
                .iter()
                .map(|s| s["attention"].as_array().map_or(0, |a| a.len() as u64))
                .sum();
            match v["state"].as_str() {
                Some("attention") => (
                    Tone::Attention,
                    if attention == 1 {
                        "1 thing needs attention".to_string()
                    } else {
                        format!("{attention} things need attention")
                    },
                ),
                Some("starting") => (Tone::Busy, "A stack starts or stops".to_string()),
                Some("ok") => (Tone::Ok, format!("{} running", plural(running, "stack"))),
                _ => (Tone::Idle, "No stack runs".to_string()),
            }
        }
    };
    entries.push(Entry::Label(head.clone()));
    if let Some(e) = error {
        entries.push(Entry::Label(format!("⚠ {}", one_line(e, 100))));
    }
    let hub_runs = matches!(hub, Hub::Running(_));
    entries.push(action("Open Dashboard", "dashboard".into(), hub_runs));
    entries.push(Entry::Separator);
    if let Hub::Running(v) = hub {
        let stacks = v["stacks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if stacks.is_empty() {
            entries.push(Entry::Label(
                "No stacks yet: `devstack up` in a repository adds one".into(),
            ));
        }
        for s in stacks {
            entries.push(stack_menu(s));
        }
        if !stacks.is_empty() {
            entries.push(Entry::Separator);
        }
    }
    if *hub == Hub::Stopped {
        entries.push(action("Start Dashboard Hub", "hub-start".into(), true));
        entries.push(Entry::Separator);
    }
    entries.push(action("Quit Tray", QUIT.into(), true));
    Menu {
        tone,
        tooltip: format!("devstack: {head}"),
        entries,
    }
}

fn stack_menu(s: &Value) -> Entry {
    let id = s["id"].as_str().unwrap_or("");
    let name = one_line(s["name"].as_str().unwrap_or(id), 60);
    let state = s["state"].as_str().unwrap_or("");
    let running = s["running"] == true;
    let (mark, said) = match state {
        "ok" => (Tone::Ok, "running"),
        "attention" => (Tone::Attention, "needs attention"),
        "starting" => (Tone::Busy, "starting"),
        "stopping" => (Tone::Busy, "stopping"),
        _ => (Tone::Idle, "stopped"),
    };
    let mut children = Vec::new();
    if running {
        children.push(Entry::Label(format!(
            "Services: {} of {} ready",
            s["ready"].as_u64().unwrap_or(0),
            s["total"].as_u64().unwrap_or(0)
        )));
        let lanes = s["lanes"].as_u64().unwrap_or(0);
        if lanes > 0 {
            children.push(Entry::Label(format!("{} started", plural(lanes, "lane"))));
        }
    }
    let attention = s["attention"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if !attention.is_empty() {
        children.push(Entry::Separator);
    }
    for a in attention {
        let lane = a["lane"].as_str();
        let service = a["service"].as_str();
        let mut label = a["text"].as_str().unwrap_or("needs attention").to_string();
        if let Some(l) = lane {
            label.push_str(&format!(" (lane {l})"));
        }
        children.push(action(
            &one_line(&label, 80),
            format!("open:{}", route(id, lane, service)),
            true,
        ));
        if let Some(d) = a["detail"].as_str().filter(|d| !d.is_empty()) {
            children.push(Entry::Label(format!("    {}", one_line(d, 80))));
        }
    }
    children.push(Entry::Separator);
    children.push(action(
        "Open in Dashboard",
        format!("open:{}", route(id, None, None)),
        true,
    ));
    match state {
        "starting" | "stopping" => {}
        _ if running => children.push(action("Stop", format!("stop:{id}"), true)),
        _ => children.push(action("Start", format!("start:{id}"), true)),
    }
    children.push(action("Open Folder", format!("folder:{id}"), true));
    Entry::Submenu {
        label: format!("{name}: {said}"),
        mark: Some(mark),
        entries: children,
    }
}

/// Do what `id` says; `Err` with what to show in the menu.
fn perform(id: &str, hub: &Hub) -> Result<()> {
    let (kind, arg) = id.split_once(':').unwrap_or((id, ""));
    match kind {
        "dashboard" => crate::hubcmd::open_dashboard("/#/"),
        "open" if arg.starts_with("/#/") => crate::hubcmd::open_dashboard(arg),
        "hub-start" => devstack_hub::ensure_running()
            .map(|_| ())
            .map_err(|e| anyhow!(e)),
        "start" => post(&format!("/hub/stacks/{}/up", crate::client::enc(arg)), None),
        // As the dashboard's Stop: the main tree's services and containers;
        // lanes that run keep theirs (ADR-0060 point 5).
        "stop" => post(
            &format!("/hub/stacks/{}/down?infra=true", crate::client::enc(arg)),
            None,
        ),
        "folder" => {
            let Hub::Running(v) = hub else {
                bail!("the hub does not run");
            };
            let repo = v["stacks"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["id"] == arg)
                .and_then(|s| s["repo"].as_str())
                .ok_or_else(|| anyhow!("no such stack"))?;
            post(
                "/hub/open",
                Some(serde_json::json!({ "stack": arg, "path": repo, "with": "files" })),
            )
        }
        _ => Ok(()),
    }
}

/// Why the worker ended.
#[derive(Debug, PartialEq)]
enum End {
    /// Quit, switched off, asked to stop, or the tray is gone.
    Done,
    /// The hub runs another devstack: its tray takes over.
    HandOver(String),
}

fn worker(
    actions: Receiver<String>,
    refresh: Sender<String>,
    updates: Sender<Menu>,
    done: Arc<AtomicBool>,
) -> End {
    let error: Arc<Mutex<Option<(String, Instant)>>> = Arc::default();
    let mut last = None;
    loop {
        if done.load(Ordering::Acquire) || hubtray::is_off() || hubtray::stop_path().exists() {
            return End::Done;
        }
        let hub = fetch();
        if let Hub::Running(v) = &hub
            && let Some(theirs) = v["version"].as_str()
            && theirs != devstack_hub::VERSION
            && std::env::var(HANDED_OVER).ok().as_deref() != Some(theirs)
        {
            return End::HandOver(theirs.to_string());
        }
        let shown = {
            let mut e = error.lock().unwrap();
            if e.as_ref().is_some_and(|(_, at)| at.elapsed() > SHOW_ERROR) {
                *e = None;
            }
            e.as_ref().map(|(text, _)| text.clone())
        };
        let next = menu(&hub, shown.as_deref());
        if last.as_ref() != Some(&next) {
            if updates.send(next.clone()).is_err() {
                return End::Done;
            }
            last = Some(next);
        }
        match actions.recv_timeout(REFRESH) {
            Ok(id) if id == QUIT => return End::Done,
            Ok(id) if id == "refresh" => {}
            Ok(id) => {
                let (hub, error, refresh) = (hub.clone(), error.clone(), refresh.clone());
                std::thread::spawn(move || {
                    let result = perform(&id, &hub);
                    *error.lock().unwrap() = result.err().map(|e| {
                        eprintln!("tray: {id}: {e:#}");
                        (format!("{e:#}"), Instant::now())
                    });
                    let _ = refresh.send("refresh".into());
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return End::Done,
        }
    }
}

/// The lock that makes this the only tray of this user.
fn lock() -> Result<Option<File>> {
    let path = hubtray::lock_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(e) => bail!("{}: {e}", path.display()),
    }
}

/// `devstack tray`: the tray, until it is quit.
pub fn run() -> Result<()> {
    if let Some(why) = devstack_desktop::tray::unavailable() {
        bail!("no tray here: {why}");
    }
    let Some(lock) = lock()? else {
        println!("the tray runs already");
        return Ok(());
    };
    // A request to stop from before this tray is not for it.
    let _ = std::fs::remove_file(hubtray::stop_path());
    std::fs::write(hubtray::pid_path(), std::process::id().to_string())?;

    let (actions_tx, actions_rx) = mpsc::channel();
    let (updates_tx, updates_rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let refresh = actions_tx.clone();
    let worker = {
        let done = done.clone();
        std::thread::Builder::new()
            .name("devstack-tray".into())
            .spawn(move || worker(actions_rx, refresh, updates_tx, done))?
    };
    let initial = menu(&Hub::Stopped, None);
    let shown = devstack_desktop::tray::run(
        Menu {
            tooltip: "devstack".into(),
            entries: vec![Entry::Label("Asking the hub…".into())],
            ..initial
        },
        updates_rx,
        actions_tx.clone(),
    );
    done.store(true, Ordering::Release);
    let _ = actions_tx.send(QUIT.into());
    let end = worker
        .join()
        .map_err(|_| anyhow!("the tray's worker panicked"))?;
    let _ = std::fs::remove_file(hubtray::pid_path());
    let _ = std::fs::remove_file(hubtray::stop_path());
    drop(lock);
    shown.map_err(|e| anyhow!(e))?;
    if let End::HandOver(version) = end {
        let exe = std::env::current_exe().map(|e| devstack_desktop::stable_exe(&e))?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("tray")
            .env(HANDED_OVER, &version)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        devstack_proc::spawn_detached(&mut cmd)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn find<'a>(entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
        entries.iter().find_map(|e| match e {
            Entry::Action { id: found, .. } if found == id => Some(e),
            Entry::Submenu { entries, .. } => find(entries, id),
            _ => None,
        })
    }

    fn summary() -> Value {
        json!({
            "state": "attention",
            "version": devstack_hub::VERSION,
            "stacks": [
                { "id": "ab12", "name": "shop", "repo": "/src/shop", "running": true,
                  "state": "attention", "ready": 2, "total": 3, "lanes": 1,
                  "attention": [{ "lane": "feat x", "service": "api",
                                  "text": "api: build held for promote", "detail": "schema changed" }] },
                { "id": "cd34", "name": "blog", "repo": "/src/blog", "running": false,
                  "state": "stopped", "ready": 0, "total": 0, "lanes": 0, "attention": [] }
            ]
        })
    }

    #[test]
    fn the_menu_follows_the_hubs_summary() {
        let m = menu(&Hub::Running(summary()), None);
        assert_eq!(m.tone, Tone::Attention);
        assert_eq!(m.tooltip, "devstack: 1 thing needs attention");
        assert!(find(&m.entries, "stop:ab12").is_some());
        assert!(find(&m.entries, "start:ab12").is_none());
        assert!(find(&m.entries, "start:cd34").is_some());
        assert!(find(&m.entries, "open:/#/s/ab12/l/feat%20x/services/api").is_some());
        assert!(find(&m.entries, "hub-start").is_none());
        assert!(find(&m.entries, QUIT).is_some());
    }

    #[test]
    fn a_stopped_hub_can_be_started_and_nothing_else() {
        let m = menu(&Hub::Stopped, None);
        assert_eq!(m.tone, Tone::Idle);
        assert!(find(&m.entries, "hub-start").is_some());
        assert!(matches!(
            find(&m.entries, "dashboard"),
            Some(Entry::Action { enabled: false, .. })
        ));
        let m = menu(&Hub::Foreign, Some("boom"));
        assert_eq!(m.tone, Tone::Attention);
        assert!(find(&m.entries, "hub-start").is_none());
        assert!(m.entries.contains(&Entry::Label("⚠ boom".into())));
    }

    #[test]
    fn a_stack_starting_or_stopping_offers_neither() {
        let mut v = summary();
        v["stacks"][1]["state"] = json!("starting");
        let m = menu(&Hub::Running(v), None);
        assert!(find(&m.entries, "start:cd34").is_none());
        assert!(find(&m.entries, "stop:cd34").is_none());
    }

    #[test]
    fn only_dashboard_routes_are_opened() {
        assert!(perform("open:https://example.invalid", &Hub::Stopped).is_ok());
        assert!(perform("nonsense", &Hub::Stopped).is_ok());
        assert!(perform("folder:ab12", &Hub::Stopped).is_err());
    }
}
