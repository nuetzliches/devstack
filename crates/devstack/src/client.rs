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

/// What this process is to the daemon (`X-Devstack-Client`, ADR-0067):
/// `cli` unless the MCP server says otherwise at its start.
static CLIENT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// This process serves MCP: its requests say `mcp` (ADR-0067).
pub fn serve_as(client: &'static str) {
    let _ = CLIENT.set(client);
}

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
    // The same directory under another name: macOS's temporary directories
    // are under /var, a link to /private/var, and a shell's working
    // directory is the real path.
    norm(a) == norm(b)
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
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
    #[allow(clippy::too_many_arguments)]
    pub fn join_lane(
        &mut self,
        wt: &Worktree,
        workers: bool,
        force: bool,
        on: Option<&str>,
        profile: Option<&str>,
        within: Option<Duration>,
        mut progress: impl FnMut(&Value),
    ) -> Result<String> {
        let (status, body) = self.call_raw(
            "POST",
            "/v1/lanes",
            Some(serde_json::json!({
                "path": wt.path, "infra": on.is_none(), "workers": workers, "force": force, "on": on,
                "profile": profile
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
            .header("Authorization", format!("Bearer {}", self.token))
            .header("X-Devstack-Client", *CLIENT.get().unwrap_or(&"cli"));
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

    /// The stack's journal (ADR-0067); an older daemon has none.
    pub fn journal(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.get("/v1/journal").map_err(
            |e| e.context("this daemon keeps no journal (devstack 0.25.0 or later does)"),
        )?)?)
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

/// End the process `pid` (a daemon that does not answer): Windows
/// terminates it, which closes its job objects and with them every process
/// tree it owns (ADR-0009); elsewhere `SIGTERM`, then `SIGKILL` after 5 s.
fn end_process(pid: u32) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };
        // SAFETY: plain Win32 calls on a handle we open and close here.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let ok = TerminateProcess(h, 1) != 0;
            let err = std::io::Error::last_os_error();
            CloseHandle(h);
            if ok { Ok(()) } else { Err(err) }
        }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: a signal to one process id.
        if unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let t0 = Instant::now();
        while pid_alive(pid) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if pid_alive(pid) {
            // SAFETY: as above.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        Ok(())
    }
}

/// Nothing listens (yet): the connection was refused, unlike a daemon that
/// accepted and did not answer.
fn is_refused(e: &anyhow::Error) -> bool {
    let t = format!("{e:#}").to_lowercase();
    t.contains("refused") || t.contains("10061")
}

pub fn error_text(body: &str) -> String {
    let text = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    daemon_older_than_file(&text, devstack_config::VERSION).unwrap_or(text)
}

/// A daemon older than the stack file it loads says "Update devstack"
/// (`LoadError::TooOld`). When this devstack (`ours`) is new enough, what is
/// old is the running daemon, which the lanes run in too: restarting the
/// main tree's daemon is what helps, as for a route it does not know.
fn daemon_older_than_file(text: &str, ours: &str) -> Option<String> {
    const UPDATE: &str = ". Update devstack (README, \"Install\").";
    let (file, rest) = text.split_once(" needs devstack ")?;
    let (required, rest) = rest.split_once(" or newer, this is ")?;
    let (daemon, after) = rest.split_once(UPDATE)?;
    let order = devstack_config::version_order;
    if order(ours)? < order(required)? {
        return None;
    }
    Some(format!(
        "{file} needs devstack {required} or newer; the running daemon is {daemon}, this devstack is {ours}. \
         Restart the main working tree's daemon to run it: \"Restart stack\" in the dashboard, or \
         `devstack down` + `devstack up` in the main working tree (`devstack down --all` while lanes run). \
         The main tree's services stop for a moment.{after}"
    ))
}

/// Start the daemon in the background unless it runs; wait until it answers.
pub fn ensure_daemon(stack_file: &Path, state_dir: &Path) -> Result<Client> {
    ensure_daemon_with(stack_file, state_dir, None)
}

/// [`ensure_daemon`]; a daemon it starts runs the main tree with `profile`
/// (ADR-0041).
pub fn ensure_daemon_with(
    stack_file: &Path,
    state_dir: &Path,
    profile: Option<&str>,
) -> Result<Client> {
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
    match profile {
        Some(p) => cmd.env("DEVSTACK_PROFILE", p),
        None => cmd.env_remove("DEVSTACK_PROFILE"),
    };
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
            // While that one's process lives, or something holds the state
            // lock (a daemon that has not written `daemon.json` yet: a slow
            // Docker engine), it is still starting: waited for up to the
            // limit below.
            Some((at, status))
                if at.elapsed() > EXITED_GRACE
                    && !devstack_daemon::read_info(state_dir)
                        .is_some_and(|i| i.pid != child.id() && pid_alive(i.pid))
                    && devstack_proc::StateLock::acquire(state_dir).is_ok() =>
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

/// `devstack status`: the table, a note when the generated agent skills no
/// longer match, and where the dashboard shows this tree.
pub fn print_status_for(stack: &Value, stack_file: &Path) {
    print_status(stack);
    let c = Client::probe(&state_dir(stack_file).unwrap_or_default())
        .ok()
        .flatten();
    if let Some(n) = c.as_ref().and_then(lanes_note) {
        println!("\n{n}");
    }
    if let Some(n) = c.as_ref().and_then(links_note) {
        println!("\n{n}");
    }
    if let Some(n) = crate::skills::outdated_note(stack_file) {
        println!("\n{n}");
    }
    let lane = c.as_ref().and_then(Client::lane).filter(|l| *l != "-");
    println!(
        "\n{}",
        crate::hubcmd::dashboard_line(&daemon_file(stack_file), lane)
    );
}

/// The service table and the infrastructure, as `devstack status` shows them.
/// `SERVICE` is a `[services.*]` entry only, as in the stack file and in
/// "main ports"; Compose containers have their own table, by group (#184).
/// How long ago `at` (Unix ms) was.
fn since(at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    crate::poolcmd::ago(now.saturating_sub(at) / 1000)
}

/// The last `n` entries of the journal (ADR-0067), oldest first.
pub fn journal_text(entries: &Value, n: usize) -> String {
    use std::fmt::Write;
    let all = entries.as_array().cloned().unwrap_or_default();
    if all.is_empty() {
        return "nothing changed at runtime yet\n".into();
    }
    let mut out = format!(
        "{:<14} {:<10} {:<14} {:<6} WHAT\n",
        "WHEN", "VIA", "LANE", "STATUS"
    );
    for e in &all[all.len().saturating_sub(n)..] {
        let what = format!(
            "{} {}{}",
            e["method"].as_str().unwrap_or(""),
            e["path"].as_str().unwrap_or(""),
            e["error"]
                .as_str()
                .map(|r| format!(": {}", truncate(r.lines().next().unwrap_or(""), 80)))
                .unwrap_or_default()
        );
        let _ = writeln!(
            out,
            "{:<14} {:<10} {:<14} {:<6} {what}",
            since(e["at"].as_u64().unwrap_or(0)),
            e["via"].as_str().unwrap_or("?"),
            e["lane"].as_str().unwrap_or("main"),
            e["status"].as_u64().unwrap_or(0),
        );
    }
    out
}

pub fn status_text(stack: &Value) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    // Who gave a lane the main ports (ADR-0067): a user's choice, not a fault.
    let m = &stack["main_served"];
    if let Some(lane) = m["lane"].as_str() {
        let _ = writeln!(
            out,
            "main ports: served by lane `{lane}`, set {}{} (`devstack promote --lane main` gives them back)\n",
            match m["via"].as_str() {
                Some("auto") => "by devstack".to_string(),
                Some("unknown") | None => "by an unknown client".to_string(),
                Some(v) => format!("in the {v}"),
            },
            m["at"]
                .as_u64()
                .map(|a| format!(", {}", since(a)))
                .unwrap_or_default()
        );
    }
    if let Some(p) = stack["profile"].as_str() {
        let _ = writeln!(out, "profile: {p} (ADR-0041)\n");
    }
    if stack["config_changed"].as_bool() == Some(true) {
        let _ = writeln!(
            out,
            "! the stack file changed since the daemon started; it applies with `devstack down` + `devstack up`\n"
        );
    }
    if let Some(e) = stack["watcher_error"].as_str() {
        let _ = writeln!(out, "! {e}\n");
    }
    // What no request reports (ADR-0050).
    for n in stack["notices"].as_array().into_iter().flatten() {
        if let Some(t) = n["text"].as_str() {
            let _ = writeln!(out, "! {t}\n");
        }
    }
    let services = stack["services"].as_array().filter(|s| !s.is_empty());
    let groups = stack["infra"].as_array().filter(|g| !g.is_empty());
    // A stack of containers only has no service table.
    if services.is_some() || groups.is_none() {
        let _ = writeln!(
            out,
            "{:<22} {:<38} {:<17} {:>6} {:>7}",
            "SERVICE", "STATE", "BUILD", "PORT", "PID"
        );
    }
    for s in services.into_iter().flatten() {
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
        // Stopped by someone: who, and when (ADR-0067).
        let a = &s["last_action"];
        if s["status"]["state"] == "stopped"
            && let (Some(action), Some(via), Some(at)) =
                (a["action"].as_str(), a["via"].as_str(), a["at"].as_u64())
        {
            let _ = writeln!(out, "  -> last `{action}` via {via}, {}", since(at));
        }
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
    if groups.is_some() {
        if services.is_some() {
            out.push('\n');
        }
        let _ = writeln!(out, "{:<22} {:<24} PORTS", "CONTAINER", "STATE");
    }
    for g in groups.into_iter().flatten() {
        let _ = writeln!(
            out,
            "compose.{} ({})",
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
            let state = format!("{}{health}", c["state"].as_str().unwrap_or(""));
            let row = format!(
                "  {:<20} {state:<24} {}",
                c["service"].as_str().unwrap_or(""),
                ports.join(", ")
            );
            let _ = writeln!(out, "{}", row.trim_end());
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
/// What `--profile` asks of a start (ADR-0041): its command, checked
/// against the stack file before anything starts.
pub struct ProfileStart {
    pub name: String,
    pub command: Option<String>,
    pub fresh: bool,
}

pub fn profile_start(
    stack_file: &Path,
    profile: Option<&str>,
    run: bool,
) -> Result<Option<ProfileStart>> {
    let Some(name) = profile else { return Ok(None) };
    let stack = devstack_config::load(stack_file)?;
    stack.with_profile(name).map_err(|issues| {
        anyhow!(
            "{}",
            issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    let p = &stack.profiles[name];
    if run && p.command.is_none() {
        bail!("the profile `{name}` names no `command` to run (`[profiles.{name}] command = ...`)");
    }
    Ok(Some(ProfileStart {
        name: name.to_string(),
        command: p.command.clone(),
        fresh: p.data == Some(devstack_config::model::DataMode::Fresh),
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn up(
    stack_file: &Path,
    workers: bool,
    force: bool,
    on: Option<&str>,
    profile: Option<&str>,
    run: bool,
    yes: bool,
) -> Result<()> {
    // Resolves whether this is a lane's worktree.
    let sd = state_dir(stack_file)?;
    let wanted = profile_start(stack_file, profile, run)?;
    // In a worktree, no `--on`: where this machine starts lanes by default
    // (ADR-0025); `--on local` keeps it here. The main tree runs here.
    let explicit = on.filter(|o| !devstack_node::member::is_here(o));
    let resolved = if worktree().is_some() {
        devstack_daemon::remote::effective_on(on)
    } else {
        explicit.map(str::to_string)
    };
    let on = resolved.as_deref();
    if let Some(o) = explicit
        && on.is_none()
        && worktree().is_some()
    {
        println!("`{o}` is this machine: the lane runs here, as with `--on local`");
    }
    if on.is_some() && worktree().is_none() {
        bail!(
            "--on runs a worktree's lane on another machine: run it in a `git worktree` of the repository"
        );
    }
    // A lane on a node needs no Docker here: everything of it runs there.
    if on.is_none() {
        ensure_engine(stack_file, |l| println!("{l}"))?;
    }
    // In the main tree the daemon runs it; a fresh start there throws away
    // the data that usually matters (ADR-0041).
    let main_profile = wanted.as_ref().filter(|_| worktree().is_none());
    if let Some(p) = main_profile
        && p.fresh
        && !yes
    {
        bail!(
            "the profile `{}` starts the main working tree with fresh data: its containers' volumes go. Confirm with --yes, or run it in a lane",
            p.name
        );
    }
    let mut c = ensure_daemon_with(
        &daemon_file(stack_file),
        &sd,
        main_profile.map(|p| p.name.as_str()),
    )?;
    if worktree().is_none() {
        let runs = c.stack()?["profile"].as_str().map(str::to_string);
        if runs.as_deref() != profile {
            bail!(
                "the main working tree runs {}: `devstack down` first, then `devstack up{}`",
                runs.map_or("without a profile".to_string(), |p| format!(
                    "with the profile `{p}`"
                )),
                profile
                    .map(|p| format!(" --profile {p}"))
                    .unwrap_or_default()
            );
        }
    }
    // Started already, here or on a node: nothing restarts it (#314).
    if c.lane().is_some_and(|l| l != "-") && (workers || force) {
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
        let name = c
            .join_lane(wt, workers, force, on, profile, None, |m| {
                if m["step"] != 0 {
                    println!("{}", step_line(m));
                }
            })
            .map_err(|e| match on {
                Some(node) => anyhow!(
                    "{e:#}
{}",
                    refused_ways(node, explicit.is_none())
                ),
                None => e,
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
            if run && let Some(cmd) = wanted.as_ref().and_then(|p| p.command.as_deref()) {
                println!("\nrunning `{cmd}`, the profile's command");
                println!("{}", run_command_on(&c, cmd, yes)?);
            }
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
    // What starts is what the profile the tree runs with says (ADR-0041).
    let stack_cfg = match c.stack()?["profile"].as_str() {
        Some(p) => stack_cfg.with_profile(p).unwrap_or(stack_cfg),
        None => stack_cfg,
    };
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
        // A lane serves the main ports: what it took over is stopped here
        // and stays so (a restart gives a carried lane the main slot back
        // right after `up`, ADR-0052); waiting for it would never end.
        let served_elsewhere = stack["main_served_by"].is_string();
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
                    "stopped" if served_elsewhere => {}
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
        Some(s) => c.get(&logs_path(&c, s, &format!("tail={tail}&grep={q}"))?)?,
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
        // A container's logs know no builds and no kept files.
        Some(s) if f.build.is_none() && f.file.is_none() => {
            c.get(&logs_path(&c, s, &f.query())?)?
        }
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
        let running = if l["removing"] == true {
            "removing".to_string()
        } else if l["active"] == false {
            "stopped".to_string()
        } else {
            format!("{}/{}", l["running"], l["services"])
        };
        let name = l["name"].as_str().unwrap_or("");
        if let Some(p) = l["profile"].as_str() {
            troubles.push(format!("{name}: runs with the profile `{p}`"));
        }
        if l["workers"] == true && l["owns_main"] != true {
            troubles.push(format!("{name}: runs its background workers"));
        }
        if let Some(e) = l["node_error"].as_str() {
            troubles.push(format!("{name}: {e}"));
        }
        if l["removing"] == true {
            troubles.push(format!(
                "{name}: its worktree is gone; the lane stops and its data is removed"
            ));
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
        if let Some(t) = not_forwarded_line(&l["not_forwarded"], l["not_forwarded_reason"].as_str())
        {
            troubles.push(format!("{name}: {t}"));
        }
        // A lane on a node: its worktree's links to a copy here (ADR-0063).
        for k in l["links"].as_array().into_iter().flatten() {
            troubles.push(format!(
                "{name}: {} {}{}",
                k["path"].as_str().unwrap_or(""),
                k["state"].as_str().unwrap_or(""),
                k["why"]
                    .as_str()
                    .map(|w| format!(": {w}"))
                    .unwrap_or_default()
            ));
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

/// A lane's `not_forwarded` (`["5353/udp"]`) as a line: `UDP port 5353 is
/// not forwarded ...`, with the daemon's reason (ADR-0056); `None` when
/// there is none (#192).
fn not_forwarded_line(ports: &Value, reason: Option<&str>) -> Option<String> {
    let ports: Vec<(String, String)> = ports
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(|p| {
            let (port, proto) = p.split_once('/').unwrap_or((p, "udp"));
            (proto.to_ascii_uppercase(), port.to_string())
        })
        .collect();
    let (first, rest) = ports.split_first()?;
    let listed = if rest.iter().all(|(proto, _)| *proto == first.0) {
        let numbers: Vec<&str> = ports.iter().map(|(_, n)| n.as_str()).collect();
        let s = if numbers.len() == 1 { "" } else { "s" };
        format!("{} port{s} {}", first.0, numbers.join(", "))
    } else {
        let each: Vec<String> = ports.iter().map(|(p, n)| format!("{p} {n}")).collect();
        format!("ports {}", each.join(", "))
    };
    let are = if ports.len() == 1 { "is" } else { "are" };
    // A daemon before ADR-0056 gives no reason: it relays no UDP.
    let reason = reason.unwrap_or(if ports.iter().any(|(p, _)| p == "UDP") {
        "an older devstack forwards TCP only"
    } else {
        "devstack forwards TCP and UDP only"
    });
    Some(format!(
        "{listed} {are} not forwarded to this machine ({reason})"
    ))
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
            "lane `{name}` is shared with {} (while it runs on a node of the pool): they open its services and see its state, logs and events, read only",
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
    println!(
        "lane `{name}` stopped; `devstack lane start {name}` (or `devstack up` in its worktree) starts it again"
    );
    Ok(())
}

/// A lane started from wherever this runs (#103): its name and its
/// node's version warning, if any; `client` then talks to it.
pub struct StartedLane {
    pub client: Client,
    pub name: String,
    pub file: PathBuf,
    pub warning: Option<String>,
    /// What its `--profile` asks (its command), checked before it started.
    pub wanted: Option<ProfileStart>,
}

/// Start the lane `name` (its name or branch, see `lane list`) of this
/// repository, here or on a node (`on`), from the main working tree or any
/// worktree: the daemon is started if it does not run. `step` hears a start
/// on a node step by step. `profile` (and with `run` its command) is
/// checked against the lane's own stack file before anything starts.
#[allow(clippy::too_many_arguments)]
pub fn start_lane(
    stack_file: &Path,
    name: &str,
    on: Option<&str>,
    profile: Option<&str>,
    run: bool,
    within: Option<Duration>,
    step: impl FnMut(&Value),
) -> Result<StartedLane> {
    let main_file = daemon_file(stack_file);
    // No `on`: where this machine starts lanes by default (ADR-0025).
    let resolved = devstack_daemon::remote::effective_on(on);
    let on = resolved.as_deref();
    // A lane on a node needs no Docker here: everything of it runs there.
    if on.is_none() {
        ensure_engine(&main_file, |l| println!("{l}"))?;
    }
    let mut c = ensure_daemon(&main_file, &state_dir(stack_file)?)?;
    let lanes: Vec<Value> = serde_json::from_str(&c.get("/v1/lanes")?)?;
    let lane = lanes
        .iter()
        .find(|l| l["name"] == name)
        .or_else(|| lanes.iter().find(|l| l["branch"] == name))
        .ok_or_else(|| {
            let known: Vec<&str> = lanes.iter().filter_map(|l| l["name"].as_str()).collect();
            anyhow!(
                "no lane `{name}`: {} (a lane is a `git worktree` of the repository)",
                if known.is_empty() {
                    "the repository has no other worktree".to_string()
                } else {
                    format!("the lanes are {}", known.join(", "))
                }
            )
        })?;
    let path = PathBuf::from(lane["path"].as_str().unwrap_or_default());
    // Its own stack file says which services it starts; its branch may
    // differ from the main tree's.
    let file = main_file
        .file_name()
        .map(|f| path.join(f))
        .filter(|f| f.is_file())
        .unwrap_or_else(|| main_file.clone());
    // Its branch's profiles, before it starts (ADR-0041).
    let wanted = profile_start(&file, profile, run)?;
    let wt = Worktree {
        path,
        main_file: main_file.clone(),
    };
    let name = c.join_lane(&wt, false, false, on, profile, within, step)?;
    let warning = on.and_then(|_| {
        c.call_raw("GET", "/v1/lanes", None)
            .ok()
            .and_then(|(_, body)| serde_json::from_str::<Vec<Value>>(&body).ok())
            .and_then(|ls| ls.into_iter().find(|l| l["name"] == name.as_str()))
            .and_then(|l| l["node_warning"].as_str().map(str::to_string))
    });
    Ok(StartedLane {
        client: c,
        name,
        file,
        warning,
        wanted,
    })
}

/// `devstack lane start <lane> [--on <node>]`: as `devstack up` in that
/// worktree, from anywhere in the repository.
pub fn lane_start(
    stack_file: &Path,
    name: &str,
    on: Option<&str>,
    profile: Option<&str>,
    run: bool,
) -> Result<()> {
    if let Some(node) = on {
        println!("leasing, syncing and starting the lane on {node}...");
    }
    let started = start_lane(stack_file, name, on, profile, run, None, |m| {
        if m["step"] != 0 {
            println!("{}", step_line(m));
        }
    })?;
    if let Some(w) = &started.warning {
        eprintln!("warning: {w}");
    }
    let wanted = started.wanted.as_ref();
    println!("lane `{}` started", started.name);
    let t0 = Instant::now();
    let outcome = start_and_wait(&started.client, &started.file, None, |name, st| {
        println!("{:>6.1}s  {name:<22} {st}", t0.elapsed().as_secs_f64());
    })?;
    match outcome {
        UpOutcome::Ready(stack) => {
            println!();
            print!("{}", status_text(&stack));
            println!(
                "\n{}",
                crate::hubcmd::dashboard_line(&daemon_file(stack_file), Some(&started.name))
            );
            if run && let Some(cmd) = wanted.and_then(|p| p.command.as_deref()) {
                println!(
                    "\nrunning `{cmd}` in lane `{}`, the profile's command",
                    started.name
                );
                println!("{}", run_command_on(&started.client, cmd, false)?);
            }
            Ok(())
        }
        UpOutcome::Failed { service, reason } => {
            eprintln!("\n{service} failed:\n{reason}");
            std::process::exit(1);
        }
        UpOutcome::TimedOut(_) => unreachable!("no timeout given"),
    }
}

/// The lane of the worktree the CLI runs in, started or not, by the
/// daemon's list; `None` in the main working tree, or without a daemon.
pub fn worktree_lane(stack_file: &Path) -> Option<String> {
    let c = Client::probe(&state_dir(stack_file).ok()?).ok().flatten()?;
    let wt = worktree()?;
    let lanes: Vec<Value> = serde_json::from_str(&c.get("/v1/lanes").ok()?).ok()?;
    let me = devstack_proc::long_path(&wt.path);
    lanes
        .iter()
        .find(|l| {
            l["path"]
                .as_str()
                .is_some_and(|p| same_dir(&devstack_proc::long_path(Path::new(p)), &me))
        })
        .and_then(|l| l["name"].as_str().map(str::to_string))
}

/// In the worktree of a lane on a node, for `status`: what `prepare` makes,
/// linked into the worktree from a copy on this machine, per path
/// (ADR-0063).
pub fn links_note(c: &Client) -> Option<String> {
    let lane = c.lane().filter(|l| *l != "-")?;
    let (status, body) = c.call_raw("GET", "/v1/lanes", None).ok()?;
    if status >= 400 {
        return None;
    }
    let lanes: Vec<Value> = serde_json::from_str(&body).ok()?;
    let links: Vec<String> = lanes
        .iter()
        .find(|l| l["name"] == lane)?
        .get("links")?
        .as_array()?
        .iter()
        .map(|k| {
            format!(
                "  {} {}{}",
                k["path"].as_str().unwrap_or(""),
                k["state"].as_str().unwrap_or(""),
                k["why"]
                    .as_str()
                    .map(|w| format!(": {w}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    (!links.is_empty()).then(|| format!("linked into this worktree:\n{}", links.join("\n")))
}

/// One line on the lanes for `status` in the main working tree (#103): that
/// worktrees exist, which run, and how to start one beside the main tree.
pub fn lanes_note(c: &Client) -> Option<String> {
    if c.lane().is_some() {
        return None;
    }
    let lanes: Vec<Value> = serde_json::from_str(&c.get("/v1/lanes").ok()?).ok()?;
    if lanes.is_empty() {
        return None;
    }
    let started: Vec<String> = lanes
        .iter()
        .filter(|l| l["active"] != false)
        .map(|l| {
            let name = l["name"].as_str().unwrap_or("");
            match l["node"].as_str() {
                Some(node) => format!("{name} on {node}"),
                None => name.to_string(),
            }
        })
        .collect();
    Some(format!(
        "lanes: {} worktree(s), {} started{}; `devstack lane list`, `devstack lane start <lane>`",
        lanes.len(),
        started.len(),
        if started.is_empty() {
            String::new()
        } else {
            format!(" ({})", started.join(", "))
        }
    ))
}

/// What `lane reset` and `lane prune` found and did, one line each.
fn print_pruned(list: &[Value], dry_run: bool) {
    for p in list {
        let size = p["bytes"]
            .as_u64()
            .map(|b| format!(", {} MiB", b >> 20))
            .unwrap_or_default();
        let done = match (p["removed"].as_bool(), p["error"].as_str()) {
            (_, Some(e)) => format!("not removed: {e}"),
            (Some(true), _) => "removed".to_string(),
            _ if dry_run => "would go".to_string(),
            _ => "kept".to_string(),
        };
        println!(
            "{:<24} {:<20} {}{size}: {done}",
            p["lane"].as_str().unwrap_or(""),
            p["place"].as_str().unwrap_or(""),
            p["why"].as_str().unwrap_or("")
        );
    }
}

pub fn lane_reset(stack_file: &Path, name: &str) -> Result<()> {
    let c = main_client(stack_file)?;
    let (status, body) = c.call_raw("POST", &format!("/v1/lanes/{name}/reset"), None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    let list: Vec<Value> = serde_json::from_str(&body)?;
    print_pruned(&list, false);
    println!("lane `{name}` starts fresh next time; its worktree and builds stay");
    Ok(())
}

pub fn lane_prune(stack_file: &Path, older_than: Option<&str>, dry_run: bool) -> Result<()> {
    let c = main_client(stack_file)?;
    let (status, body) = c.call_raw(
        "POST",
        "/v1/lanes/prune",
        Some(serde_json::json!({ "older_than": older_than, "dry_run": dry_run })),
    )?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    let list: Vec<Value> = serde_json::from_str(&body)?;
    if list.is_empty() {
        println!("nothing to prune: every lane's data belongs to a worktree");
        return Ok(());
    }
    print_pruned(&list, dry_run);
    if dry_run {
        println!("(dry run: nothing removed; without --dry-run these go)");
    }
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
    let shown = c.get(&logs_path(&c, name, &format!("tail={tail}"))?)?;
    print!("{}", for_terminal(&shown));
    if !shown.is_empty() {
        println!();
    }
    Ok(())
}

/// The Compose group of `name` when it names a container and no service:
/// its logs come from `/v1/infra/{group}/{name}/…`, which a lane's node
/// answers for a lane there (its daemon has no Docker, #101).
pub fn container_group(c: &Client, name: &str) -> Result<Option<String>> {
    let stack = c.stack()?;
    let is_service = stack["services"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["name"] == name);
    if is_service {
        return Ok(None);
    }
    Ok(stack["infra"]
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
        .and_then(|g| g["group"].as_str())
        .map(str::to_string))
}

/// The path of `name`'s logs with `query` (`tail`, `grep`, `since`): a
/// container's, or a service's.
pub fn logs_path(c: &Client, name: &str, query: &str) -> Result<String> {
    Ok(match container_group(c, name)? {
        Some(g) => format!("/v1/infra/{g}/{name}/logs?{query}"),
        None => format!("/v1/services/{name}/logs?{query}"),
    })
}

/// `devstack logs <name> --follow [--grep]`: the last `tail` lines, then
/// every new one, from the daemon's log stream (a service's, or a Compose
/// container's by its service name).
pub fn follow_logs(c: &Client, name: &str, tail: usize, grep: Option<&str>) -> Result<()> {
    let path = match container_group(c, name)? {
        Some(g) => format!("/v1/infra/{g}/{name}/logs/stream?tail={tail}"),
        None => format!("/v1/services/{name}/logs/stream?tail={tail}"),
    };
    let needle = grep.map(str::to_lowercase);
    use std::io::Write as _;
    let mut closed = false;
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
        // A closed pipe (`| head`) ends it.
        closed = !(ok && out.flush().is_ok());
        !closed
    })?;
    if !closed {
        eprintln!("the daemon ended the log stream (stopped?)");
    }
    Ok(())
}

pub fn run_command(stack_file: &Path, name: &str, yes: bool) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    println!("{}", run_command_on(&c, name, yes)?);
    Ok(())
}

/// Run command `name` where `c` points (the main tree or a lane) and follow
/// it to its end: its output, or an error with it.
pub fn run_command_on(c: &Client, name: &str, yes: bool) -> Result<String> {
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
            let log = c.get(&format!("/v1/runs/{run}/log"))?;
            if info["status"] == "succeeded" {
                return Ok(log);
            }
            bail!(
                "{log}\ncommand `{name}` failed: {}",
                info["error"].as_str().unwrap_or("")
            );
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

pub fn down(stack_file: &Path, infra: bool, all: bool) -> Result<()> {
    let sd = state_dir(stack_file)?;
    let c = match Client::probe(&sd) {
        Ok(Some(c)) => c,
        Ok(None) => {
            println!("no daemon is running for this stack");
            return Ok(());
        }
        // It runs but refuses (a token that changed since it started) or
        // hangs: end it by its pid, or nothing could.
        Err(e) => return end_unanswering_daemon(&sd, e, all),
    };
    if c.lane() == Some("-") {
        println!("this worktree's lane is not started: nothing to stop");
        return Ok(());
    }
    if let Some(lane) = c.lane() {
        // A lane stops its own services; the daemon and the infrastructure
        // belong to the main working tree.
        let (status, body) = c.post(&format!("/v1/down?infra={infra}"), None)?;
        if status >= 400 {
            bail!("{}", error_text(&body));
        }
        println!("lane `{lane}` stopped");
        return Ok(());
    }
    // `--all`: the lanes stop first, their containers too, so the main
    // tree's go as well (a daemon before 0.17.1 ignores it). With
    // `--keep-infra` every container stays, the lanes' too.
    let (status, body) = c.post(
        &format!("/v1/down?infra={infra}&all={}", all && infra),
        None,
    )?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    // Lanes that still run keep the daemon they live in (ADR-0024), what
    // serves them and the `shared` containers they use (ADR-0030, ADR-0068).
    let lanes = running_lanes(status, &body);
    if !lanes.is_empty() && !all {
        let list = |key: &str| -> Vec<String> {
            serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| serde_json::from_value(v[key].clone()).ok())
                .unwrap_or_default()
        };
        // What serves the lanes (`lanes = false`) runs on for them.
        let kept = list("kept");
        let containers = list("containers");
        let mut stays = vec!["the daemon".to_string()];
        if !kept.is_empty() {
            stays.push(format!("{} that serve them", kept.join(", ")));
        }
        if !infra {
            stays.push("the containers".into());
        } else if !containers.is_empty() {
            stays.push(format!("the containers {}", containers.join(", ")));
        }
        println!(
            "main working tree stopped{}; lanes keep running: {} ({} stay; `devstack down --all` stops them too)",
            if infra {
                ", its containers removed"
            } else {
                ""
            },
            lanes.join(", "),
            stays.join(", "),
        );
        return Ok(());
    }
    // A lane started since `/v1/down` answered keeps the daemon (ADR-0024),
    // unless `--all` says otherwise.
    let path = if all {
        "/v1/shutdown"
    } else {
        "/v1/shutdown?unless_lanes=true"
    };
    // A daemon on its way out may close the connection before it answers.
    if let Ok((409, body)) = c.post(path, None) {
        let lanes = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<String>>(v["lanes"].clone()).ok())
            .unwrap_or_default();
        println!(
            "main working tree stopped; a lane started meanwhile and keeps running: {} (`devstack down --all` stops it too)",
            lanes.join(", ")
        );
        return Ok(());
    }
    let t0 = Instant::now();
    while devstack_daemon::read_info(&sd).is_some() && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "stopped{}",
        if infra {
            ", containers removed (their volumes stay)"
        } else {
            "; containers keep running (`devstack down` removes them)"
        }
    );
    Ok(())
}

/// `down` when the stack's daemon runs but does not answer (`why`): it
/// refuses the token (replaced since it started) or hangs. Its process is
/// the one `daemon.json` names and it holds the stack's lock, so it is ours
/// to end: with `--all` from the main working tree (the lanes in it end with
/// it), else the error says how.
fn end_unanswering_daemon(state_dir: &Path, why: anyhow::Error, all: bool) -> Result<()> {
    let Some(info) = devstack_daemon::read_info(state_dir) else {
        return Err(why);
    };
    if worktree().is_some() {
        bail!(
            "{why:#}\nthe daemon lives in the main working tree: `devstack down --all` there ends it (pid {}), with every lane in it",
            info.pid
        );
    }
    if !all {
        bail!(
            "{why:#}\n`devstack down --all` ends it by its pid ({}), with every lane in it; its infrastructure stays",
            info.pid
        );
    }
    // Nobody holds the lock: that pid is not this stack's daemon (any more).
    if devstack_proc::StateLock::acquire(state_dir).is_ok() {
        println!("no daemon is running for this stack");
        return Ok(());
    }
    eprintln!("{why:#}");
    println!("ending the daemon by its pid ({})", info.pid);
    end_process(info.pid).with_context(|| format!("cannot end process {}", info.pid))?;
    let t0 = Instant::now();
    while pid_alive(info.pid) && t0.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if pid_alive(info.pid) {
        bail!("process {} is still there after 15 s", info.pid);
    }
    // It had no chance to say it is gone.
    let _ = std::fs::remove_file(devstack_daemon::info_path(state_dir));
    println!(
        "stopped; its services ended with it, its containers keep running (`devstack up`, then `devstack down`, removes them)"
    );
    Ok(())
}

/// What to do when a lane's start on `node` failed (#310): the error alone
/// gives no way on, and `up` again asks the same. `default`: `node` is
/// this machine's default (`devstack pool default-on`), not `--on`.
fn refused_ways(node: &str, default: bool) -> String {
    format!(
        "{}the ways on: try again once something is free; `devstack pool placement <lane>` says what is free where; \
         `devstack up --on <node>` picks another; `devstack up --on local` runs it on this machine (its memory check applies){}",
        if default {
            format!(
                "the lane did not start on `{node}`, where this machine starts lanes by default (`devstack pool default-on`).\n"
            )
        } else {
            format!("the lane did not start on `{node}`.\n")
        },
        if default {
            ": the user chose the pool as the default, so ask them before running it here"
        } else {
            ""
        }
    )
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

/// Where a lane could start (ADR-0025, ADR-0026 point 3): this machine's
/// free capacity, and per joined pool each node with its load, its lanes
/// and the coordinator's order and reason; and where lanes go by default.
/// What `devstack pool placement` prints and the MCP `placement` returns.
pub fn placement(stack_file: &Path, lane: &str) -> Result<Value> {
    use devstack_node::member;
    let stack = devstack_config::load(stack_file)?;
    let repo = stack.stack.name.clone();
    let state = devstack_daemon::remote::state_dir();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let here = serde_json::json!({
        "cpus": cores,
        "memory_free": devstack_proc::memory_available(),
        "memory_total": devstack_proc::memory_total(),
    });
    let mut pools = Vec::new();
    for (name, _) in member::pools(&state) {
        let local = devstack_daemon::remote::local_member(&name);
        let Some(me) = local
            .membership()
            .ok()
            .flatten()
            .filter(|m| m.state == devstack_node::store::State::Approved)
        else {
            continue;
        };
        let nodes = local.nodes().unwrap_or_default();
        // An older coordinator has no order to give: the same ranking from
        // its nodes' reports, without their policies (it says so).
        let mut computed_here = false;
        let order = local
            .placement(&repo, lane, devstack_node::share::Need::default(), None)
            .or_else(|e| {
                if !e.to_string().starts_with("404") {
                    return Err(e.to_string());
                }
                computed_here = true;
                let reports: Vec<(&str, &str, &devstack_node::coordinator::Report)> = nodes
                    .iter()
                    // Nodes only: a developer's machine that asks for lanes is none.
                    .filter(|n| n.online && !n.paused && n.id != me.id)
                    .filter(|n| matches!(n.kind.as_str(), "server" | "node"))
                    .filter_map(|n| Some((n.id.as_str(), n.name.as_str(), n.report.as_ref()?)))
                    .collect();
                Ok(devstack_node::coordinator::rank(
                    &reports,
                    &me.id,
                    &repo,
                    lane,
                    devstack_node::policy::floor_need(devstack_node::share::Need::default()),
                    Some(&devstack_node::version::Release::this()),
                ))
            });
        let leases = local.leases().unwrap_or_default();
        let ranked: Vec<Value> = match &order {
            Ok(list) => list
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let rep = nodes.iter().find(|n| n.id == p.node).and_then(|n| n.report.as_ref());
                    serde_json::json!({
                        "node": p.name,
                        "on": if name == member::DEFAULT_POOL { p.name.clone() } else { format!("{name}:{}", p.name) },
                        "rank": i + 1,
                        "eligible": p.eligible,
                        "why": p.why,
                        "cpus_free": p.cpus,
                        "memory_free": p.memory,
                        "cpus_total": rep.and_then(|r| r.total).map(|t| t.cpus),
                        "memory_total": rep.and_then(|r| r.total).map(|t| t.memory),
                        "cpus_used": rep.and_then(|r| r.used).map(|u| u.cpus),
                        "memory_used": rep.and_then(|r| r.used).map(|u| u.memory),
                        "disk_free": rep.and_then(|r| r.disk_free),
                        "lanes": leases.iter().filter(|l| l.node == p.node).count(),
                        "devstack": rep.and_then(|r| r.devstack.as_ref()).map(|d| d.version.clone()),
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        pools.push(serde_json::json!({
            "pool": name,
            "on": if name == member::DEFAULT_POOL { "pool".to_string() } else { name.clone() },
            "nodes": ranked,
            "error": order.err(),
            "note": computed_here.then_some("the coordinator runs an older devstack: this order is computed here from its nodes' reports, without their policies"),
        }));
    }
    Ok(serde_json::json!({
        "lane": lane,
        "repository": repo,
        "default_on": member::default_on(&state).unwrap_or_else(|| "local".into()),
        "here": here,
        "pools": pools,
    }))
}

/// `devstack pool placement <lane>`: [`placement`] as a table.
pub fn print_placement(stack_file: &Path, lane: &str) -> Result<()> {
    let p = placement(stack_file, lane)?;
    let gib = |v: &Value| {
        v.as_u64().map_or("-".into(), |b| {
            format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
        })
    };
    println!(
        "lane `{lane}` of `{}`; without --on it starts: {}",
        p["repository"].as_str().unwrap_or(""),
        p["default_on"].as_str().unwrap_or("local")
    );
    println!(
        "here: {} cores, {} of {} memory free",
        p["here"]["cpus"],
        gib(&p["here"]["memory_free"]),
        gib(&p["here"]["memory_total"])
    );
    for pool in p["pools"].as_array().into_iter().flatten() {
        println!(
            "\npool `{}` (--on {}):",
            pool["pool"].as_str().unwrap_or(""),
            pool["on"].as_str().unwrap_or("")
        );
        if let Some(e) = pool["error"].as_str() {
            println!("  no order from the coordinator: {e}");
            continue;
        }
        if let Some(n) = pool["note"].as_str() {
            println!("  ({n})");
        }
        for n in pool["nodes"].as_array().into_iter().flatten() {
            println!(
                "  {}. {:<20} {:>5.1} CPU / {:>9} free, {} lane(s), disk {}  {}{}",
                n["rank"],
                n["node"].as_str().unwrap_or(""),
                n["cpus_free"].as_f64().unwrap_or(0.0),
                gib(&n["memory_free"]),
                n["lanes"],
                gib(&n["disk_free"]),
                if n["eligible"] == true {
                    ""
                } else {
                    "cannot take it: "
                },
                n["why"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_start_on_the_pool_names_the_ways_on() {
        let by_default = refused_ways("pool", true);
        assert!(
            by_default.contains("default-on")
                && by_default.contains("--on local")
                && by_default.contains("ask them"),
            "{by_default}"
        );
        let asked = refused_ways("node-1", false);
        assert!(
            asked.contains("--on local") && !asked.contains("ask them"),
            "{asked}"
        );
    }

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
    fn the_status_says_when_the_file_watcher_misses_edits() {
        let stack = serde_json::json!({ "services": [], "watcher_error": "the file watcher lost events (overflow)" });
        assert!(status_text(&stack).starts_with("! the file watcher lost events (overflow)\n"));
        assert!(!status_text(&serde_json::json!({ "services": [] })).contains('!'));
    }

    #[test]
    fn containers_are_listed_apart_from_services() {
        let container = serde_json::json!({ "group": "app", "project": "shop", "containers": [
            { "service": "db", "state": "running", "health": "healthy", "ports": ["127.0.0.1:3306"] },
            { "service": "migrate", "state": "exited", "ports": [] }
        ] });
        // Containers only: no service table, nothing under `SERVICE`.
        let only = status_text(&serde_json::json!({ "services": [], "infra": [container] }));
        assert_eq!(
            only,
            "CONTAINER              STATE                    PORTS\n\
             compose.app (shop)\n  \
             db                   running (healthy)        127.0.0.1:3306\n  \
             migrate              exited\n"
        );
        // Both: the service table, then the containers'.
        let both = status_text(&serde_json::json!({
            "services": [{ "name": "api", "status": { "state": "stopped" } }],
            "infra": [container]
        }));
        let s = both.find("SERVICE ").unwrap();
        let api = both.find("\napi ").unwrap();
        let c = both.find("\n\nCONTAINER ").unwrap();
        assert!(s < api && api < c, "{both}");
        // Neither: the empty service table.
        assert!(status_text(&serde_json::json!({ "services": [] })).starts_with("SERVICE "));
    }

    #[test]
    fn the_status_lists_the_trees_notices() {
        let stack = serde_json::json!({ "services": [], "notices": [
            { "id": "copy", "level": "warn", "text": "The lane's copy is behind.", "since": 1 }
        ] });
        assert!(status_text(&stack).starts_with("! The lane's copy is behind.\n"));
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
    fn a_daemon_older_than_the_file_is_restarted_not_updated() {
        // As an older daemon words it (`LoadError::TooOld`).
        let old = devstack_config::LoadError::TooOld {
            path: "wt/devstack.toml".into(),
            required: "0.9.0".into(),
            have: "0.8.0".into(),
        }
        .to_string();
        assert!(old.contains("Update devstack"), "{old}");
        // This devstack is new enough: the daemon has to restart.
        let text = daemon_older_than_file(&old, "0.9.0").unwrap();
        assert!(
            text.starts_with("wt/devstack.toml needs devstack 0.9.0 or newer")
                && text.contains("the running daemon is 0.8.0, this devstack is 0.9.0")
                && text.contains("`devstack down` + `devstack up` in the main working tree")
                && text.contains("\"Restart stack\"")
                && !text.contains("Update devstack"),
            "{text}"
        );
        // Through `error_text`, as the daemon answers it.
        let body = serde_json::json!({ "error": format!("lane `feat`: {old}") }).to_string();
        let ours = devstack_config::VERSION;
        let mine = devstack_config::LoadError::TooOld {
            path: "wt/devstack.toml".into(),
            required: ours.into(),
            have: "0.0.1".into(),
        };
        let body_mine = serde_json::json!({ "error": mine.to_string() }).to_string();
        assert!(error_text(&body_mine).contains("Restart the main working tree's daemon"));
        assert!(error_text(&body).starts_with("lane `feat`: wt/devstack.toml"));
        // This devstack is too old as well: updating is still the answer.
        assert_eq!(daemon_older_than_file(&old, "0.8.5"), None);
        assert_eq!(daemon_older_than_file("another error", "0.9.0"), None);
    }

    #[test]
    fn this_process_is_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
    }

    #[test]
    fn a_lanes_ports_not_forwarded_are_said() {
        let line = |v: Value| not_forwarded_line(&v, None);
        assert_eq!(line(serde_json::json!(null)), None, "an older daemon");
        assert_eq!(line(serde_json::json!([])), None);
        assert_eq!(
            line(serde_json::json!(["5353/udp"])).as_deref(),
            Some(
                "UDP port 5353 is not forwarded to this machine (an older devstack forwards TCP only)"
            )
        );
        assert_eq!(
            line(serde_json::json!(["5060/udp", "5353/udp"])).as_deref(),
            Some(
                "UDP ports 5060, 5353 are not forwarded to this machine (an older devstack forwards TCP only)"
            )
        );
        assert_eq!(
            line(serde_json::json!(["9000/sctp"])).as_deref(),
            Some(
                "SCTP port 9000 is not forwarded to this machine (devstack forwards TCP and UDP only)"
            )
        );
        // The daemon's reason (ADR-0056): the node is too old to relay.
        let why = "the node build-1 runs devstack 0.19.0, which forwards TCP only: devstack 0.20.0 or later there relays UDP";
        assert_eq!(
            not_forwarded_line(&serde_json::json!(["5353/udp", "9000/sctp"]), Some(why)),
            Some(format!(
                "ports UDP 5353, SCTP 9000 are not forwarded to this machine ({why})"
            ))
        );
    }
}
