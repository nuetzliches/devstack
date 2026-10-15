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
}

impl Client {
    /// Connect to a running daemon of this stack, if there is one.
    pub fn connect(state_dir: &Path) -> Option<Self> {
        let info = devstack_daemon::read_info(state_dir)?;
        let token = devstack_daemon::token::load()?;
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let c = Self {
            base: format!("http://127.0.0.1:{}", info.port),
            token,
            agent,
        };
        c.get("/v1/stack").ok().map(|_| c)
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, String)> {
        let req = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token));
        let resp = match body {
            Some(b) => self.agent.run(
                req.header("Content-Type", "application/json")
                    .body(b.to_string())?,
            ),
            None => self.agent.run(req.body(())?),
        };
        let mut resp = resp.map_err(|e| anyhow!("daemon: {e}"))?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
    }

    pub fn get(&self, path: &str) -> Result<String> {
        let (status, body) = self.call("GET", path, None)?;
        if status >= 400 {
            bail!("{}", error_text(&body));
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

pub fn error_text(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

/// Start the daemon in the background unless it runs; wait until it answers.
pub fn ensure_daemon(stack_file: &Path, state_dir: &Path) -> Result<Client> {
    if let Some(c) = Client::connect(state_dir) {
        return Ok(c);
    }
    std::fs::create_dir_all(state_dir)?;
    // stdout/stderr (a panic, say); the daemon's own diagnostic log is daemon.log.
    let log = std::fs::File::create(state_dir.join("daemon.out"))?;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .arg("-f")
        .arg(std::path::absolute(stack_file)?)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Own console without a window, out of the terminal's job, no inherited
    // pipes (devstack_proc::spawn_detached).
    let mut child = devstack_proc::spawn_detached(&mut cmd).context("start the daemon")?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        if let Some(c) = Client::connect(state_dir) {
            return Ok(c);
        }
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "the daemon exited ({status}):\n{}",
                daemon_report(state_dir)
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "the daemon did not answer within 20 s:\n{}",
        daemon_report(state_dir)
    )
}

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

pub fn state_dir(stack_file: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(stack_file)?;
    Ok(abs
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(abs)
        .join(".devstack"))
}

pub fn need(state_dir: &Path) -> Result<Client> {
    Client::connect(state_dir)
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
            s["build_id"].as_str().unwrap_or("-"),
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
pub fn up(stack_file: &Path) -> Result<()> {
    ensure_engine(stack_file)?;
    let sd = state_dir(stack_file)?;
    let c = ensure_daemon(stack_file, &sd)?;
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

pub fn logs(stack_file: &Path, name: &str, tail: usize, follow: bool) -> Result<()> {
    let c = need(&state_dir(stack_file)?)?;
    let mut shown = c.get(&format!("/v1/services/{name}/logs?tail={tail}"))?;
    print!("{shown}");
    if !shown.is_empty() {
        println!();
    }
    if !follow {
        return Ok(());
    }
    loop {
        std::thread::sleep(Duration::from_millis(700));
        let now = c.get(&format!("/v1/services/{name}/logs?tail=100000"))?;
        if now.len() > shown.len() && now.starts_with(&shown) {
            print!("{}", &now[shown.len()..]);
        } else if now != shown && !shown.ends_with(&now) {
            println!("{now}"); // a new instance started
        }
        shown = now;
    }
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

pub fn down(stack_file: &Path, infra: bool) -> Result<()> {
    let sd = state_dir(stack_file)?;
    let Some(c) = Client::connect(&sd) else {
        println!("no daemon is running for this stack");
        return Ok(());
    };
    let (status, body) = c.post(&format!("/v1/down?infra={infra}"), None)?;
    if status >= 400 {
        bail!("{}", error_text(&body));
    }
    let _ = c.post("/v1/shutdown", None);
    let t0 = Instant::now();
    while devstack_daemon::read_info(&sd).is_some() && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "stopped{}",
        if infra {
            ", infrastructure removed"
        } else {
            "; infrastructure keeps running (`down --infra` to stop it)"
        }
    );
    Ok(())
}

/// A stack with Compose groups needs the Docker engine. Start Docker Desktop
/// when it is installed and not running, instead of letting every compose call
/// fail with a message about a pipe.
fn ensure_engine(stack_file: &Path) -> Result<()> {
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
            println!("Docker is not running; started {name}, waiting for it (up to 3 min) ...");
            let version = engine::wait("docker", Duration::from_secs(180)).map_err(|e| {
                anyhow::anyhow!("{e}; start {name} yourself and run `devstack up` again")
            })?;
            println!("Docker {version} is ready.");
            Ok(())
        }
    }
}
