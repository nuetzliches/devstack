//! The CLI as a client of the daemon's API (ADR-0007): it implements no
//! lifecycle logic of its own.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
    /// The daemon's version from `daemon.json` (empty: a daemon older than
    /// the field).
    version: String,
    /// `/v1/lanes/<name>` when the CLI runs in a lane's working tree: every
    /// call goes to that lane's API (plan §3.5).
    prefix: String,
}

/// A linked `git worktree` of a repository whose main working tree has a
/// stack file: it runs as a lane of the main tree's daemon (plan §3.5).
#[derive(Debug, Clone)]
pub struct Worktree {
    /// The worktree's root.
    pub path: PathBuf,
    /// The main working tree's stack file.
    pub main_file: PathBuf,
}

static WORKTREE: std::sync::OnceLock<Option<Worktree>> = std::sync::OnceLock::new();

/// The worktree the CLI runs in, if it is a lane (decided once per process).
pub fn worktree() -> Option<&'static Worktree> {
    WORKTREE.get().and_then(Option::as_ref)
}

/// Two spellings of one directory (`D:/x` from git, `D:\X` from a shell).
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

/// `<repo>/.git` is a file `gitdir: <main>/.git/worktrees/<name>` in a linked
/// worktree; the main working tree is then `<main>`.
pub fn main_tree_of(repo: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(repo.join(".git")).ok()?;
    let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let dir = if dir.is_relative() {
        repo.join(dir)
    } else {
        dir
    };
    let worktrees = dir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let git = worktrees.parent()?;
    if git.file_name()? != ".git" {
        return None;
    }
    Some(git.parent()?.to_path_buf())
}

/// How long a call that should answer at once may take (the liveness probe):
/// a daemon that accepts and never answers is reported, not waited for.
const PROBE: Duration = Duration::from_secs(10);

impl Client {
    /// Connect to a running daemon of this stack, if there is one.
    pub fn connect(state_dir: &Path) -> Option<Self> {
        Self::probe(state_dir).ok().flatten()
    }

    /// `Ok(None)`: no daemon runs (no `daemon.json`, or its process is gone).
    /// `Err`: one runs but does not answer (it hangs, or its port belongs to
    /// a process that accepts and never answers).
    pub fn probe(state_dir: &Path) -> Result<Option<Self>> {
        let Some(info) = devstack_daemon::read_info(state_dir) else {
            return Ok(None);
        };
        if !pid_alive(info.pid) {
            return Ok(None); // a stale daemon.json: the daemon was killed
        }
        if info.stopping {
            return Ok(None); // on its way out: `ensure_daemon` waits for it
        }
        let Some(token) = devstack_daemon::token::load() else {
            return Ok(None);
        };
        // Long enough for a lane's move to a node, which the daemon answers
        // when done (it waits up to 30 minutes itself). A start on a node
        // answers at once and is followed.
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(1900)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let mut c = Self {
            base: format!("http://127.0.0.1:{}", info.port),
            token,
            agent,
            version: info.version,
            prefix: String::new(),
        };
        match c.call_timeout("GET", "/v1/stack", None, Some(PROBE)) {
            Ok((s, _)) if s < 400 => {}
            Ok((s, body)) => bail!(
                "the daemon (pid {}, port {}) refused: {} (HTTP {s})",
                info.pid,
                info.port,
                error_text(&body)
            ),
            Err(e) if is_refused(&e) => return Ok(None), // starting, or just gone
            Err(e) => bail!(
                "the daemon of this stack (pid {}, port {}) does not answer: {e:#}; \
                 if it hangs, end that process and run `devstack up` again",
                info.pid,
                info.port
            ),
        }
        if let Some(wt) = worktree() {
            // The lane of this worktree, if it is started (`up` starts it).
            let lanes: Vec<Value> = serde_json::from_str(&c.get("/v1/lanes")?)?;
            let me = devstack_proc::long_path(&wt.path);
            if let Some(l) = lanes.iter().find(|l| {
                l["active"] != false
                    && l["path"]
                        .as_str()
                        .map(|p| same_dir(&devstack_proc::long_path(Path::new(p)), &me))
                        == Some(true)
            }) {
                c.prefix = format!("/v1/lanes/{}", l["name"].as_str().unwrap_or(""));
            } else {
                c.prefix = "/v1/lanes/-".into(); // not a lane yet: every call says so
            }
        }
        Ok(Some(c))
    }

    /// The lane this client talks to, if any.
    pub fn lane(&self) -> Option<&str> {
        self.prefix.strip_prefix("/v1/lanes/")
    }

    /// Register this worktree as a lane (and make sure the shared
    /// infrastructure runs); from then on calls go to it.
    ///
    /// On a node (`on`) the daemon answers `202` once the lane is leased
    /// and goes on syncing and starting it there: this follows the lane's
    /// `moving` until it runs there (or its start failed, an error), at most
    /// `within`, and tells `progress` each new step.
    pub fn join_lane(
        &mut self,
        wt: &Worktree,
        workers: bool,
        force: bool,
        on: Option<&str>,
        within: Option<Duration>,
        mut progress: impl FnMut(&Value),
    ) -> Result<String> {
        let (status, body) = self.call_raw(
            "POST",
            "/v1/lanes",
            Some(serde_json::json!({
                "path": wt.path, "infra": on.is_none(), "workers": workers, "force": force, "on": on
            })),
        )?;
        if status >= 400 {
            bail!("{}", error_text(&body));
        }
        let name = serde_json::from_str::<Value>(&body)?["name"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if status == 202 {
            self.follow_start(&name, within, &mut progress)?;
        }
        self.prefix = format!("/v1/lanes/{name}");
        Ok(name)
    }

    /// Follow the start of lane `name` on a node until it runs there; its
    /// error if it failed.
    fn follow_start(
        &self,
        name: &str,
        within: Option<Duration>,
        progress: &mut dyn FnMut(&Value),
    ) -> Result<()> {
        let t0 = Instant::now();
        let mut last = Value::Null;
        loop {
            let lanes: Vec<Value> = serde_json::from_str(&self.get("/v1/lanes")?)?;
            let Some(lane) = lanes.into_iter().find(|l| l["name"] == name) else {
                bail!("lane `{name}` is gone (its worktree was removed?)");
            };
            let m = &lane["moving"];
            if m.is_object() {
                if let Some(e) = m["error"].as_str() {
                    bail!("{e}");
                }
                if *m != last {
                    progress(m);
                    last = m.clone();
                }
            } else if lane["active"] == true {
                return Ok(());
            } else {
                bail!("lane `{name}`: its start on the node ended, and it does not run");
            }
            if within.is_some_and(|w| t0.elapsed() > w) {
                bail!(
                    "lane `{name}` is still starting on its node after {} s ({}); it continues: \
                     `devstack lane list` shows how far it got",
                    t0.elapsed().as_secs(),
                    m["what"].as_str().unwrap_or("")
                );
            }
            std::thread::sleep(Duration::from_millis(700));
        }
    }

    /// The path a call goes to: in a lane's worktree, the lane's API.
    fn route(&self, path: &str) -> Result<String> {
        if self.prefix == "/v1/lanes/-"
            && path.starts_with("/v1/")
            && !path.starts_with("/v1/lanes")
        {
            bail!("this worktree's lane is not started: run `devstack up` here");
        }
        Ok(
            if path.starts_with("/v1/") && !path.starts_with("/v1/lanes") {
                format!("{}{path}", self.prefix)
            } else {
                path.to_string()
            },
        )
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, String)> {
        self.call_raw(method, &self.route(path)?, body)
    }

    /// Without the lane prefix: the main daemon's own routes.
    pub fn call_raw(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, String)> {
        self.call_timeout(method, path, body, None)
    }

    /// One request; `timeout` replaces the agent's long one, `None` for
    /// it, `Some(None)` for no timeout at all (a stream).
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        timeout: Option<Option<Duration>>,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let req = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token));
        let req = match body {
            Some(b) => req
                .header("Content-Type", "application/json")
                .body(b.to_string())?,
            None => req.body(String::new())?,
        };
        let resp = match timeout {
            Some(t) => {
                let req = self.agent.configure_request(req).timeout_global(t).build();
                self.agent.run(req)
            }
            None => self.agent.run(req),
        };
        resp.map_err(|e| anyhow!("daemon: {e}"))
    }

    /// A 404 without a body is a route the daemon does not have (an older
    /// daemon): said here, for every method.
    fn call_timeout(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<(u16, String)> {
        let mut resp = self.request(method, path, body, timeout.map(Some))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if status == 404 && text.trim().is_empty() {
            bail!("{}", self.no_route(path));
        }
        Ok((status, text))
    }

    /// Why the daemon has no route for `path`.
    fn no_route(&self, path: &str) -> String {
        no_route_text(path, &self.version)
    }

    /// Follow a server-sent event stream (no timeout) until `on_event`
    /// returns false or the daemon ends it.
    pub fn stream(&self, path: &str, on_event: impl FnMut(Value) -> bool) -> Result<()> {
        let path = self.route(path)?;
        let mut resp = self.request("GET", &path, None, Some(None))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            if status == 404 && body.trim().is_empty() {
                bail!("{}", self.no_route(&path));
            }
            bail!("{} (HTTP {status})", error_text(&body));
        }
        let reader = std::io::BufReader::new(resp.into_body().into_reader());
        read_sse(reader, on_event)?;
        Ok(())
    }

    pub fn get(&self, path: &str) -> Result<String> {
        let (status, body) = self.call("GET", path, None)?;
        if status >= 400 {
            bail!("{} (HTTP {status})", error_text(&body));
        }
        Ok(body)
    }

    pub fn post(&self, path: &str, body: Option<Value>) -> Result<(u16, String)> {
        self.call("POST", path, body)
    }

    pub fn stack(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.get("/v1/stack")?)?)
    }
}

fn no_route_text(path: &str, daemon_version: &str) -> String {
    let route = path.split('?').next().unwrap_or(path);
    let ours = devstack_config::VERSION;
    match daemon_version {
        "" => format!(
            "the running daemon does not know `{route}`; it is older than this devstack ({ours}): restart the stack (`devstack down` + `devstack up`)"
        ),
        v if v != ours => format!(
            "the running daemon is devstack {v}, this is {ours}, and it does not know `{route}`: restart the stack (`devstack down` + `devstack up`)"
        ),
        _ => format!("the daemon has no `{route}` (HTTP 404)"),
    }
}

/// Server-sent events: the JSON of every `data:` event; comments (keep-alive)
/// are skipped. Ends when the stream ends or `on_event` returns false.
pub fn read_sse(
    mut reader: impl std::io::BufRead,
    mut on_event: impl FnMut(Value) -> bool,
) -> std::io::Result<()> {
    let mut data = String::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            if !data.is_empty() {
                let event = serde_json::from_str::<Value>(&data);
                data.clear();
                if let Ok(v) = event
                    && !on_event(v)
                {
                    return Ok(());
                }
            }
        } else if let Some(d) = text.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
}

/// Whether the process `pid` still runs.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE,
        };
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls on a handle we open and close here.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                // Refused: it exists, but belongs to someone else.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe {
            libc::kill(pid as i32, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }
}

/// Nothing listens (yet): the connection was refused, unlike a daemon that
/// accepted and did not answer.
fn is_refused(e: &anyhow::Error) -> bool {
    let t = format!("{e:#}").to_lowercase();
    t.contains("refused") || t.contains("10061")
}

pub fn error_text(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

/// Start the daemon in the background unless it runs; wait until it answers.
pub fn ensure_daemon(stack_file: &Path, state_dir: &Path) -> Result<Client> {
    // A daemon that runs but does not answer is an error: a second one would
    // only fail on its state lock.
    if let Some(c) = Client::probe(state_dir)? {
        return Ok(c);
    }
    wait_for_stopping(state_dir);
    std::fs::create_dir_all(state_dir)?;
    // stdout/stderr (a panic, say); the daemon's own diagnostic log is
    // daemon.log. Appended: a second `up` at the same moment must not empty
    // the file the first daemon writes to; started anew once it is large.
    let out = state_dir.join("daemon.out");
    let large = std::fs::metadata(&out).is_ok_and(|m| m.len() > 1024 * 1024);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!large)
        .truncate(large)
        .open(&out)?;
    let exe = std::env::current_exe()?;
    let stack_file = std::path::absolute(stack_file)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .arg("-f")
        .arg(&stack_file)
        // Not the caller's directory: a daemon started from a worktree would
        // hold it for its lifetime, and Windows refuses `git worktree remove`.
        .current_dir(stack_file.parent().unwrap_or(state_dir))
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Own console without a window, out of the terminal's job, no inherited
    // pipes (devstack_proc::spawn_detached).
    let mut child = devstack_proc::spawn_detached(&mut cmd).context("start the daemon")?;
    let t0 = Instant::now();
    let mut exited: Option<(Instant, std::process::ExitStatus)> = None;
    let mut noted = false;
    loop {
        if let Some(c) = Client::connect(state_dir) {
            return Ok(c);
        }
        match exited {
            // Another `up` may have started a daemon at the same moment, and
            // ours gave up on the state lock: give that one a few seconds.
            // While that one's process lives, it is still starting.
            Some((at, status))
                if at.elapsed() > EXITED_GRACE
                    && !devstack_daemon::read_info(state_dir)
                        .is_some_and(|i| i.pid != child.id() && pid_alive(i.pid)) =>
            {
                bail!(
                    "the daemon exited ({status}):\n{}",
                    daemon_report(state_dir)
                )
            }
            Some(_) => {}
            None => {
                if let Ok(Some(status)) = child.try_wait() {
                    exited = Some((Instant::now(), status));
                }
            }
        }
        // It binds its port once it knows the state of the infrastructure,
        // which a Docker engine that is just starting takes a while to tell.
        if !noted && exited.is_none() && t0.elapsed() > Duration::from_secs(20) {
            eprintln!(
                "note: the daemon is still starting (a slow Docker engine?); waiting up to 2 minutes"
            );
            noted = true;
        }
        if t0.elapsed() > Duration::from_secs(120) {
            bail!(
                "the daemon did not answer within 2 minutes; it may still come up (`devstack status` tells):\n{}",
                daemon_report(state_dir)
            )
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A daemon on its way out (`down`, then `up` at once) holds the state lock
/// until its services stopped, longer than a new one waits for it: wait
/// here, up to as long as it gives its services, instead of starting one
/// that gives up.
fn wait_for_stopping(state_dir: &Path) {
    let stopping =
        || devstack_daemon::read_info(state_dir).is_some_and(|i| i.stopping && pid_alive(i.pid));
    let t0 = Instant::now();
    let mut noted = false;
    while stopping() && t0.elapsed() < Duration::from_secs(120) {
        if !noted && t0.elapsed() > Duration::from_secs(2) {
            eprintln!("note: the previous daemon of this stack is still stopping; waiting for it");
            noted = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// How long `ensure_daemon` still looks for a daemon after its own exited.
const EXITED_GRACE: Duration = Duration::from_secs(8);

/// What a daemon that did not come up left behind: its output and the end of
/// its diagnostic log.
fn daemon_report(state_dir: &Path) -> String {
    let out = std::fs::read_to_string(state_dir.join("daemon.out")).unwrap_or_default();
    let log: Vec<String> = devstack_daemon::diag::tail(&state_dir.join("daemon.log"), 20, None)
        .iter()
        .map(|l| devstack_daemon::diag::pretty(l))
        .collect();
    format!(
        "{}\n--- {} (last lines)\n{}",
        out.trim_end(),
        state_dir.join("daemon.log").display(),
        log.join("\n")
    )
}

/// Where the daemon of this stack keeps its state. In a lane's worktree:
/// the main working tree's (its daemon runs the lane).
pub fn state_dir(stack_file: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(stack_file)?;
    let repo = abs.parent().map(Path::to_path_buf).unwrap_or(abs.clone());
    let wt = WORKTREE.get_or_init(|| {
        let main = main_tree_of(&repo)?;
        let main_file = main.join(abs.file_name()?);
        main_file.is_file().then(|| Worktree {
            path: repo.clone(),
            main_file,
        })
    });
    Ok(match wt {
        Some(w) => w
            .main_file
            .parent()
            .unwrap_or(Path::new("."))
            .join(".devstack"),
        None => repo.join(".devstack"),
    })
}

/// The stack file the daemon runs from: the main working tree's in a lane.
pub fn daemon_file(stack_file: &Path) -> PathBuf {
    let _ = state_dir(stack_file);
    match worktree() {
        Some(w) => w.main_file.clone(),
        None => stack_file.to_path_buf(),
    }
}

pub fn need(state_dir: &Path) -> Result<Client> {
    Client::probe(state_dir)?
        .ok_or_else(|| anyhow!("no daemon is running for this stack (start it with `devstack up`)"))
}

pub fn state_of(s: &Value) -> String {
    let st = &s["status"];
    match st["state"].as_str().unwrap_or("?") {
        "failed" => format!(
            "failed: {}",
            st["reason"]
                .as_str()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
        ),
        "unhealthy" => format!("unhealthy: {}", st["reason"].as_str().unwrap_or("")),
        "exited" | "completed" => {
            format!("{} ({})", st["state"].as_str().unwrap(), st["exit_code"])
        }
        "building" => match st["build_id"].as_str() {
            Some(id) => format!("building {id}"),
            None => "building".into(),
        },
        other => other.to_string(),
    }
}

pub fn print_status(stack: &Value) {
    print!("{}", status_text(stack));
}

/// `devstack status`: the table, plus a note when the generated agent skills
/// no longer match.
pub fn print_status_for(stack: &Value, stack_file: &Path) {
    print_status(stack);
    if let Some(n) = crate::skills::outdated_note(stack_file) {
        println!("\n{n}");
    }
}

/// The service table and the infrastructure, as `devstack status` shows them.
pub fn status_text(stack: &Value) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if stack["config_changed"].as_bool() == Some(true) {
        let _ = writeln!(
            out,
            "! the stack file changed since the daemon started; it applies with `devstack down` + `devstack up`\n"
        );
    }
    let _ = writeln!(
        out,
        "{:<22} {:<38} {:<17} {:>6} {:>7}",
        "SERVICE", "STATE", "BUILD", "PORT", "PID"
    );
    for s in stack["services"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "{:<22} {:<38} {:<17} {:>6} {:>7}",
            s["name"].as_str().unwrap_or(""),
            truncate(&state_of(s), 38),
            if s["hot_reload"] == true {
                "hot reload"
            } else {
                s["build_id"].as_str().unwrap_or("-")
            },
            s["port"].as_u64().map_or("-".into(), |p| p.to_string()),
            s["pid"].as_u64().map_or("-".into(), |p| p.to_string()),
        );
        // A newer build on its way, or why it did not arrive: the running one
        // is not the latest change (ADR-0003).
        let c = &s["candidate"];
        if c.is_object() {
            let reason = c["reason"]
                .as_str()
                .and_then(|r| r.lines().find(|l| !l.trim().is_empty()))
                .map(|r| format!(": {}", r.trim()))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  -> not yet active: {} {}{}",
                c["state"].as_str().unwrap_or("?"),
                c["build_id"].as_str().unwrap_or(""),
                truncate(&reason, 100)
            );
        }
    }
    for g in stack["infra"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "\ncompose.{} ({})",
            g["group"].as_str().unwrap_or(""),
            g["project"].as_str().unwrap_or("")
        );
        if let Some(e) = g["error"].as_str() {
            let _ = writeln!(out, "  error: {e}");
        }
        for c in g["containers"].as_array().into_iter().flatten() {
            let health = c["health"]
                .as_str()
                .filter(|h| !h.is_empty())
                .map(|h| format!(" ({h})"))
                .unwrap_or_default();
            let ports: Vec<&str> = c["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str())
                .collect();
            let _ = writeln!(
                out,
                "  {:<20} {}{health}  {}",
                c["service"].as_str().unwrap_or(""),
                c["state"].as_str().unwrap_or(""),
                ports.join(", ")
            );
            let lost: Vec<&str> = c["unreachable"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str())
                .collect();
            if !lost.is_empty() {
                let _ = writeln!(
                    out,
                    "    ! {} closes connections from this machine at once: Docker lost the port forward (restart the container), or nothing listens inside",
                    lost.join(", ")
                );
            }
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.into()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// `devstack up`: infra, then autostart services; follow until all are ready
/// or one fails. Exit code 1 on failure.
pub fn up(stack_file: &Path, workers: bool, force: bool, on: Option<&str>) -> Result<()> {
    // Resolves whether this is a lane's worktree.
    let sd = state_dir(stack_file)?;
    if on.is_some() && worktree().is_none() {
        bail!(
            "--on runs a worktree's lane on another machine: run it in a `git worktree` of the repository"
        );
    }
    // A lane on a node needs no Docker here: everything of it runs there.
    if on.is_none() {
        ensure_engine(stack_file, |l| println!("{l}"))?;
    }
    let mut c = ensure_daemon(&daemon_file(stack_file), &sd)?;
    if c.lane().is_some_and(|l| l != "-") && on.is_none() && (workers || force) {
        eprintln!(
            "note: this worktree's lane is started already; --workers and --force apply when it starts (`devstack down` here first)"
        );
    }
    if let Some(wt) = worktree()
        && (on.is_some() || c.lane().is_none_or(|l| l == "-"))
    {
        if let Some(node) = on {
            println!("leasing, syncing and starting the lane on {node}...");
        }
        // A start on a node answers once leased; its steps follow.
        let name = c.join_lane(wt, workers, force, on, None, |m| {
            if m["step"] != 0 {
                println!("{}", step_line(m));
            }
        })?;
        // `pool`: the coordinator chose; say which node, and whether it
        // runs another devstack (known once the start is followed).
        let view = on.and_then(|_| {
            c.call_raw("GET", "/v1/lanes", None)
                .ok()
                .and_then(|(_, body)| serde_json::from_str::<Vec<Value>>(&body).ok())
                .and_then(|ls| ls.into_iter().find(|l| l["name"] == name.as_str()))
        });
        let placed = on.map(|node| {
            view.as_ref()
                .and_then(|l| l["node"].as_str().map(str::to_string))
                .unwrap_or_else(|| node.to_string())
        });
        if let Some(w) = view.as_ref().and_then(|l| l["node_warning"].as_str()) {
            eprintln!("warning: {w}");
        }
        match placed.as_deref() {
            Some(node) => println!(
                "this worktree runs as lane `{name}` of {} on {node}",
                wt.main_file.display()
            ),
            None => println!(
                "this worktree runs as lane `{name}` of {}",
                wt.main_file.display()
            ),
        }
    }
    crate::hubcmd::ensure_quietly();
    let t0 = Instant::now();
    let outcome = start_and_wait(&c, stack_file, None, |name, st| {
        println!("{:>6.1}s  {name:<22} {st}", t0.elapsed().as_secs_f64());
    })?;
    match outcome {
        UpOutcome::Ready(stack) => {
            println!();
            print_status_for(&stack, stack_file);
            Ok(())
        }
        UpOutcome::Failed { service, reason } => {
            eprintln!("\n{service} failed:\n{reason}");
            std::process::exit(1);
        }
        UpOutcome::TimedOut(_) => unreachable!("no timeout given"),
    }
}

pub enum UpOutcome {
    /// Every autostart service is ready; the stack view at that moment.
    Ready(Value),
    Failed {
        service: String,
        reason: String,
    },
    /// Still starting when the timeout ran out; the stack view at that moment.
    TimedOut(Value),
}

/// `POST /v1/up`, then follow the stack until every autostart service is
/// ready or one fails. `progress` hears every change of a service's state.
pub fn start_and_wait(
    c: &Client,
    stack_file: &Path,
    timeout: Option<Duration>,
    mut progress: impl FnMut(&str, &str),
) -> Result<UpOutcome> {
    let (status, body) = c.post("/v1/up", None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    let stack_cfg = devstack_config::load(stack_file)?;
    let expected: Vec<String> = stack_cfg
        .services
        .iter()
        .filter(|(_, s)| {
            s.autostart
                && (s.platforms.is_empty()
                    || s.platforms
                        .contains(&devstack_config::model::Platform::current()))
                && (c.lane().is_none() || s.lanes)
        })
        .map(|(n, _)| n.clone())
        .collect();
    let mut last: std::collections::BTreeMap<String, String> = Default::default();
    let t0 = Instant::now();
    loop {
        let stack = c.stack()?;
        let mut done = true;
        let mut failed: Vec<(String, String)> = Vec::new();
        for s in stack["services"].as_array().into_iter().flatten() {
            let name = s["name"].as_str().unwrap_or("").to_string();
            let st = state_of(s);
            if last.get(&name) != Some(&st) {
                progress(&name, &st);
                last.insert(name.clone(), st);
            }
            if expected.contains(&name) {
                let state = s["status"]["state"].as_str().unwrap_or("");
                match state {
                    "healthy" | "running" => {}
                    "completed" if s["status"]["exit_code"] == 0 => {}
                    "failed" | "exited" => failed.push((
                        name.clone(),
                        s["status"]["reason"].as_str().unwrap_or("").to_string(),
                    )),
                    _ => done = false,
                }
            }
        }
        // Wait until every autostart service is ready or failed: one failure
        // does not stop the others (the daemon starts what does not depend on it).
        if done && !failed.is_empty() {
            return Ok(UpOutcome::Failed {
                service: failed
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                reason: failed
                    .iter()
                    .map(|(n, r)| format!("{n}: {r}"))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            });
        }
        if done {
            return Ok(UpOutcome::Ready(stack));
        }
        if timeout.is_some_and(|t| t0.elapsed() >= t) {
            return Ok(UpOutcome::TimedOut(stack));
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

pub fn service_action(stack_file: &Path, name: &str, action: &str) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let (status, body) = c.post(&format!("/v1/services/{name}/{action}"), None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    print_status(&serde_json::from_str(&body)?);
    Ok(())
}

/// `devstack logs --daemon` / `--hub`: the diagnostic log, readable or raw.
/// Reads the file directly, so it works after the daemon is gone too.
pub fn diag_log(
    path: &Path,
    tail: usize,
    grep: Option<&str>,
    follow: bool,
    json: bool,
) -> Result<()> {
    use devstack_daemon::diag;
    let show = |l: &str| {
        if json {
            println!("{l}");
        } else {
            println!("{}", diag::pretty(l));
        }
    };
    let lines = diag::tail(path, tail, grep);
    if lines.is_empty() && !path.exists() {
        println!("no log yet at {}", path.display());
    }
    for l in &lines {
        show(l);
    }
    if !follow {
        return Ok(());
    }
    let mut seen = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let needle = grep.map(str::to_lowercase);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if len < seen {
            seen = 0; // rotated
        }
        if len == seen {
            continue;
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(seen))?;
        let mut text = String::new();
        f.read_to_string(&mut text)?;
        seen = len;
        for l in text.lines() {
            if needle.as_ref().is_none_or(|g| l.to_lowercase().contains(g)) {
                show(l);
            }
        }
    }
}

/// `devstack logs [service] --grep X`: matching lines of one service, or of
/// every service (`service | line`).
pub fn logs_grep(stack_file: &Path, service: Option<&str>, grep: &str, tail: usize) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let q = enc(grep);
    let text = match service {
        Some(s) => c.get(&format!("/v1/services/{s}/logs?tail={tail}&grep={q}"))?,
        None => c.get(&format!("/v1/logs?tail={tail}&grep={q}"))?,
    };
    if text.is_empty() {
        println!("no line contains `{grep}`");
    } else {
        println!("{}", for_terminal(&text));
    }
    Ok(())
}

/// Which kept logs `logs_kept` reads.
pub struct LogFilter<'a> {
    pub tail: usize,
    pub grep: Option<&'a str>,
    pub build: Option<&'a str>,
    pub since: Option<&'a str>,
    pub file: Option<&'a str>,
}

impl LogFilter<'_> {
    pub fn query(&self) -> String {
        let mut q = format!("tail={}", self.tail);
        for (k, v) in [
            ("grep", self.grep),
            ("build", self.build),
            ("since", self.since),
            ("file", self.file),
        ] {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                q.push_str(&format!("&{k}={}", enc(v)));
            }
        }
        q
    }
}

/// Earlier logs of a service (by build, by time, or one file), or with
/// `since` and `grep` and no service a search across every service.
pub fn logs_kept(stack_file: &Path, service: Option<&str>, f: LogFilter) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let text = match service {
        Some(s) => c.get(&format!("/v1/services/{s}/logs?{}", f.query()))?,
        None if f.grep.is_some() && f.build.is_none() && f.file.is_none() => {
            c.get(&format!("/v1/logs?{}", f.query()))?
        }
        None => {
            anyhow::bail!("name a service (without one, only --grep with --since searches all)")
        }
    };
    if text.is_empty() {
        println!("no kept log matches");
    } else {
        println!("{}", for_terminal(&text));
    }
    Ok(())
}

/// The builds of a service as text, newest first (also the MCP tool `builds`).
pub fn builds_text(list: &[Value]) -> String {
    if list.is_empty() {
        return "no build in the diagnostic log yet".into();
    }
    list.iter()
        .map(|b| {
            let at = jiff::Timestamp::from_millisecond(b["started_ms"].as_i64().unwrap_or(0))
                .map(|t| {
                    t.to_zoned(jiff::tz::TimeZone::system())
                        .strftime("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            let secs = (b["updated_ms"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(b["started_ms"].as_u64().unwrap_or(0)))
                / 1000;
            let id = b["build"].as_str().unwrap_or("");
            let mut line = format!(
                "{at}  {:<12}  {:<10}  {secs:>4} s{}",
                short_id(id),
                b["outcome"].as_str().unwrap_or(""),
                if b["current"].as_bool() == Some(true) {
                    "  (running now)"
                } else {
                    ""
                }
            );
            if let Some(r) = b["reason"].as_str() {
                line.push_str(&format!("\n    {}", r.lines().next().unwrap_or("")));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn builds(stack_file: &Path, service: &str, limit: usize) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let list: Vec<Value> =
        serde_json::from_str(&c.get(&format!("/v1/services/{service}/builds?limit={limit}"))?)?;
    println!("{}", builds_text(&list));
    Ok(())
}

/// The kept logs of a service, newest first.
pub fn log_files(stack_file: &Path, service: &str) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let files: Vec<Value> =
        serde_json::from_str(&c.get(&format!("/v1/services/{service}/logs/files"))?)?;
    if files.is_empty() {
        println!("`{service}` has no kept log");
    }
    for f in files {
        let at = jiff::Timestamp::from_millisecond(f["started_ms"].as_i64().unwrap_or(0))
            .map(|t| {
                t.to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let build = f["build"].as_str().map(short_id).unwrap_or("-");
        let port = f["port"]
            .as_u64()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{at}  {:<5}  build {build:<12}  port {port:<5}  {:>8}  {}{}",
            f["kind"].as_str().unwrap_or(""),
            human_bytes(f["bytes"].as_u64().unwrap_or(0)),
            f["file"].as_str().unwrap_or(""),
            if f["current"].as_bool() == Some(true) {
                "  (running now)"
            } else {
                ""
            },
        );
    }
    Ok(())
}

/// The first 12 characters of a build id (never splits a character).
fn short_id(id: &str) -> &str {
    id.char_indices().nth(12).map_or(id, |(i, _)| &id[..i])
}

fn human_bytes(b: u64) -> String {
    match b {
        0..1024 => format!("{b} B"),
        1024..1_048_576 => format!("{:.0} KB", b as f64 / 1024.0),
        _ => format!("{:.1} MB", b as f64 / 1_048_576.0),
    }
}

pub(crate) fn main_client(stack_file: &Path) -> Result<Client> {
    need(&state_dir(stack_file)?)
}

pub fn lane_list(stack_file: &Path) -> Result<()> {
    let c = main_client(stack_file)?;
    let lanes: Vec<Value> = serde_json::from_str(&c.get("/v1/lanes")?)?;
    if lanes.is_empty() {
        println!("no lanes: this repository has no other worktree (`git worktree add` makes one)");
        return Ok(());
    }
    println!(
        "{:<24} {:<24} {:>8} {:>9}  {:<5}  {:<16} PATH",
        "LANE", "BRANCH", "PORTS +", "RUNNING", "MAIN", "RUNS ON"
    );
    let mut troubles = Vec::new();
    for l in lanes {
        let running = if l["active"] == false {
            "stopped".to_string()
        } else {
            format!("{}/{}", l["running"], l["services"])
        };
        let name = l["name"].as_str().unwrap_or("");
        if let Some(e) = l["node_error"].as_str() {
            troubles.push(format!("{name}: {e}"));
        }
        // A start on a node or a move under way, or one that failed lately.
        let m = &l["moving"];
        if m.is_object() {
            let to = m["to"].as_str().unwrap_or("");
            let kind = if m["kind"] == "move" {
                "moving to"
            } else {
                "starting on"
            };
            troubles.push(match m["error"].as_str() {
                Some(e) => format!("{name}: {kind} {to} failed: {e}"),
                None => format!("{name}: {kind} {to}{}", step_line(m)),
            });
        }
        if let Some(w) = l["node_warning"].as_str() {
            troubles.push(format!("{name}: {w}"));
        }
        println!(
            "{:<24} {:<24} {:>8} {:>9}  {:<5}  {:<16} {}",
            name,
            l["branch"].as_str().unwrap_or("(detached)"),
            l["port_offset"],
            running,
            if l["owns_main"] == true { "yes" } else { "" },
            match (l["pool"].as_str(), l["node"].as_str()) {
                (Some(p), Some(n)) if p != "default" => format!("{p}:{n}"),
                (_, Some(n)) => n.to_string(),
                _ => "local".into(),
            },
            l["path"].as_str().unwrap_or("")
        );
    }
    for t in troubles {
        println!("  ! {t}");
    }
    Ok(())
}

/// A lane's `moving` as a line: `  [2/3] sync: ...`.
fn step_line(m: &Value) -> String {
    format!(
        "  [{}/{}] {}",
        m["step"],
        m["steps"],
        m["what"].as_str().unwrap_or("")
    )
}

/// Print the steps of a lane's start on a node or its move while `done` is
/// false: the lane is the one named `name`, or the one in `path`.
fn follow_steps(
    stack_file: &Path,
    name: Option<String>,
    path: Option<PathBuf>,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let d = done.clone();
    let file = stack_file.to_path_buf();
    let handle = std::thread::spawn(move || {
        let Ok(c) = main_client(&file) else { return };
        let mut last = String::new();
        while !d.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(700));
            let Ok(text) = c.get("/v1/lanes") else {
                continue;
            };
            let Ok(lanes) = serde_json::from_str::<Vec<Value>>(&text) else {
                continue;
            };
            let lane = lanes.iter().find(|l| {
                name.as_deref().is_some_and(|n| l["name"] == n)
                    || path.as_ref().is_some_and(|p| {
                        l["path"]
                            .as_str()
                            .is_some_and(|lp| same_dir(Path::new(lp), p))
                    })
            });
            let Some(m) = lane.map(|l| &l["moving"]).filter(|m| m.is_object()) else {
                continue;
            };
            let line = step_line(m);
            if line != last && m["step"] != 0 {
                println!("{line}");
                last = line;
            }
        }
    });
    (done, handle)
}

pub fn lane_move(stack_file: &Path, name: &str, to: &str) -> Result<()> {
    let c = main_client(stack_file)?;
    println!("moving lane `{name}` to {to}: starting it there; it takes over once it is ready...");
    let t0 = Instant::now();
    let (done, steps) = follow_steps(stack_file, Some(name.to_string()), None);
    let answer = c.call_raw(
        "POST",
        &format!("/v1/lanes/{name}/move"),
        Some(serde_json::json!({ "to": to })),
    );
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = steps.join();
    let (status, body) = answer?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    println!(
        "lane `{name}` runs on {to} now ({:.0} s); its ports here are the same",
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

pub fn lane_share(stack_file: &Path, name: &str, with: &[String]) -> Result<()> {
    let c = main_client(stack_file)?;
    let (status, body) = c.call_raw(
        "POST",
        &format!("/v1/lanes/{name}/share"),
        Some(serde_json::json!({ "with": with })),
    )?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    if with.is_empty() {
        println!("lane `{name}` is shared with nobody");
    } else {
        println!(
            "lane `{name}` is shared with {} (while it runs on a node of the pool)",
            with.join(", ")
        );
    }
    Ok(())
}

pub fn lane_stop(stack_file: &Path, name: &str) -> Result<()> {
    let c = main_client(stack_file)?;
    let (status, body) = c.call_raw("POST", &format!("/v1/lanes/{name}/stop"), None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    println!("lane `{name}` stopped; `devstack up` in its worktree starts it again");
    Ok(())
}

pub fn promote_lane(stack_file: &Path, lane: &str) -> Result<()> {
    let c = main_client(stack_file)?;
    let (status, body) = c.call_raw("POST", &format!("/v1/lanes/{lane}/main"), None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    if lane == "main" {
        println!("the main working tree serves the main ports again");
    } else {
        println!(
            "lane `{lane}` serves the main ports now, dev servers included; `devstack promote --lane main` gives them back"
        );
    }
    Ok(())
}

/// Percent-encoding for a query value (no dependency for one parameter).
pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Service output for this terminal: colour codes stay where a terminal shows
/// them, and are dropped when the output goes to a file or a pipe.
pub fn for_terminal(text: &str) -> std::borrow::Cow<'_, str> {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        std::borrow::Cow::Borrowed(text)
    } else {
        devstack_daemon::ansi::strip(text)
    }
}

pub fn logs(stack_file: &Path, name: &str, tail: usize, follow: bool) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    if follow {
        return follow_logs(&c, name, tail, None);
    }
    let shown = c.get(&format!("/v1/services/{name}/logs?tail={tail}"))?;
    print!("{}", for_terminal(&shown));
    if !shown.is_empty() {
        println!();
    }
    Ok(())
}

/// `devstack logs <name> --follow [--grep]`: the last `tail` lines, then
/// every new one, from the daemon's log stream (a service's, or a Compose
/// container's by its service name).
pub fn follow_logs(c: &Client, name: &str, tail: usize, grep: Option<&str>) -> Result<()> {
    let stack = c.stack()?;
    let is_service = stack["services"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["name"] == name);
    let group = stack["infra"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|g| {
            g["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["service"] == name)
        })
        .and_then(|g| g["group"].as_str());
    let path = match group {
        Some(g) if !is_service => format!("/v1/infra/{g}/{name}/logs/stream?tail={tail}"),
        _ => format!("/v1/services/{name}/logs/stream?tail={tail}"),
    };
    let needle = grep.map(str::to_lowercase);
    use std::io::Write as _;
    c.stream(&path, |ev| {
        let mut out = std::io::stdout().lock();
        let ok = match ev["type"].as_str() {
            Some("line") => {
                let text = ev["text"].as_str().unwrap_or("");
                if needle
                    .as_ref()
                    .is_none_or(|g| text.to_lowercase().contains(g))
                {
                    writeln!(out, "{}", for_terminal(text)).is_ok()
                } else {
                    true
                }
            }
            Some("instance") => writeln!(
                out,
                "--- another instance took over (build {})",
                ev["build_id"].as_str().unwrap_or("-")
            )
            .is_ok(),
            _ => true,
        };
        ok && out.flush().is_ok() // a closed pipe (`| head`) ends it
    })?;
    eprintln!("the daemon ended the log stream (stopped?)");
    Ok(())
}

pub fn run_command(stack_file: &Path, name: &str, yes: bool) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let (mut status, mut body) = c.post(
        &format!("/v1/commands/{name}"),
        Some(serde_json::json!({ "confirm": yes })),
    )?;
    if status == 428 {
        let text = serde_json::from_str::<Value>(&body)?["confirm"]
            .as_str()
            .unwrap_or("Continue?")
            .to_string();
        // Nobody to answer (a script, an agent): say so instead of waiting.
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            bail!("`{name}` asks for confirmation: \"{text}\"; run it with --yes to confirm");
        }
        eprint!("{text} [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "j" | "J" | "ja") {
            bail!("not confirmed");
        }
        (status, body) = c.post(
            &format!("/v1/commands/{name}"),
            Some(serde_json::json!({ "confirm": true })),
        )?;
    }
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    let run = serde_json::from_str::<Value>(&body)?["run"]
        .as_u64()
        .unwrap_or(0);
    loop {
        let info: Value = serde_json::from_str(&c.get(&format!("/v1/runs/{run}"))?)?;
        if info["status"] != "running" {
            println!("{}", c.get(&format!("/v1/runs/{run}/log"))?);
            if info["status"] == "succeeded" {
                return Ok(());
            }
            bail!(
                "command `{name}` failed: {}",
                info["error"].as_str().unwrap_or("")
            );
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

pub fn down(stack_file: &Path, infra: bool, all: bool) -> Result<()> {
    let sd = state_dir(stack_file)?;
    let Some(c) = Client::connect(&sd) else {
        println!("no daemon is running for this stack");
        return Ok(());
    };
    if c.lane() == Some("-") {
        println!("this worktree's lane is not started: nothing to stop");
        return Ok(());
    }
    if let Some(lane) = c.lane() {
        // A lane stops its own services; the daemon and the infrastructure
        // belong to the main working tree.
        let (status, body) = c.post("/v1/down", None)?;
        if status >= 400 {
            bail!("{}", error_text(&body));
        }
        println!("lane `{lane}` stopped");
        return Ok(());
    }
    let (status, body) = c.post(&format!("/v1/down?infra={infra}"), None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    // Lanes that still run keep the daemon they live in (ADR-0024) and the
    // infrastructure they share (ADR-0030).
    let lanes = running_lanes(status, &body);
    if !lanes.is_empty() && !all {
        println!(
            "main working tree stopped; lanes keep running: {} (the daemon and the infrastructure stay; `devstack down --all` stops them too)",
            lanes.join(", ")
        );
        return Ok(());
    }
    let _ = c.post("/v1/shutdown", None);
    let t0 = Instant::now();
    while devstack_daemon::read_info(&sd).is_some() && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "stopped{}",
        if infra && !lanes.is_empty() {
            "; infrastructure keeps running, lanes were using it (`down --infra` to stop it)"
        } else if infra {
            ", infrastructure removed"
        } else {
            "; infrastructure keeps running (`down --infra` to stop it)"
        }
    );
    Ok(())
}

/// The lanes a `200` from the main tree's `/v1/down` names: they still run.
pub fn running_lanes(status: u16, body: &str) -> Vec<String> {
    if status != 200 {
        return Vec::new();
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| serde_json::from_value(v["lanes"].clone()).ok())
        .unwrap_or_default()
}

/// A stack with Compose groups needs the Docker engine. Start Docker Desktop
/// when it is installed and not running, instead of letting every compose call
/// fail with a message about a pipe. `progress` hears what it does (the CLI
/// prints it, the MCP tool `up` returns it).
pub fn ensure_engine(stack_file: &Path, mut progress: impl FnMut(&str)) -> Result<()> {
    use devstack_compose::engine::{self, Engine};
    let has_compose = devstack_config::load(stack_file)
        .map(|s| !s.compose.is_empty())
        .unwrap_or(false);
    if !has_compose {
        return Ok(());
    }
    match engine::status("docker") {
        Engine::Running { .. } => Ok(()),
        Engine::Missing => Err(anyhow::anyhow!(
            "this stack has Compose groups, but `docker` is not on PATH (install Docker Desktop or the Docker Engine)"
        )),
        Engine::Stopped {
            starter: None,
            hint,
        } => Err(anyhow::anyhow!("the Docker engine is not running: {hint}")),
        Engine::Stopped {
            starter: Some(_), ..
        } => {
            let name = engine::start().map_err(|e| anyhow::anyhow!(e))?;
            progress(&format!(
                "Docker is not running; started {name}, waiting for it (up to 3 min) ..."
            ));
            let version = engine::wait("docker", Duration::from_secs(180)).map_err(|e| {
                anyhow::anyhow!("{e}; start {name} yourself and run `devstack up` again")
            })?;
            progress(&format!("Docker {version} is ready."));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_sent_events_are_read_one_by_one() {
        let stream = b": keep-alive\n\ndata: {\"type\":\"line\",\"text\":\"a\"}\n\n\
data: {\"type\":\"instance\",\r\ndata: \"build_id\":\"x\"}\r\n\r\n\
data: not json\n\n\
data: {\"type\":\"line\",\"text\":\"b\"}\n\n\
data: {\"type\":\"line\",\"text\":\"c\"}\n\n";
        let mut seen = Vec::new();
        read_sse(&stream[..], |v| {
            seen.push(v);
            seen.len() < 3
        })
        .unwrap();
        assert_eq!(seen.len(), 3, "stops when told: {seen:?}");
        assert_eq!(seen[0]["text"], "a");
        assert_eq!(seen[1]["build_id"], "x", "multi-line data");
        assert_eq!(seen[2]["text"], "b", "an unparsable event is skipped");
    }

    #[test]
    fn short_ids_never_split_a_character() {
        assert_eq!(short_id("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("ääääääääääääää"), "ääääääääääää");
    }

    #[test]
    fn a_missing_route_names_the_daemon_version() {
        assert!(no_route_text("/v1/x?y=1", "").contains("older than this devstack"));
        assert!(no_route_text("/v1/x", "0.0.1").contains("is devstack 0.0.1"));
        assert_eq!(
            no_route_text("/v1/x", devstack_config::VERSION),
            "the daemon has no `/v1/x` (HTTP 404)"
        );
    }

    #[test]
    fn this_process_is_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
    }
}
