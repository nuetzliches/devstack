//! The daemon end to end, as a black box through the CLI and the HTTP API
//! (plan §10). Each test builds its own copy of the fixture stack in a
//! temporary directory with free ports, its own Compose project name and its
//! own token directory, so tests neither touch the repository nor each other.
//!
//! Opt-in, because they need the .NET 10 SDK (and Docker for the Compose
//! variant):
//!   DEVSTACK_INTEGRATION=1          services-only variant (any OS)
//!   DEVSTACK_INTEGRATION_COMPOSE=1  also the variant with a Compose group (Linux containers)

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_devstack");

fn enabled(var: &str) -> bool {
    std::env::var(var).as_deref() == Ok("1")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Stack {
    dir: tempfile::TempDir,
    token_home: tempfile::TempDir,
    port: u16,
    project: Option<String>,
}

impl Stack {
    fn new(with_compose: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture-stack");
        let api = dir.path().join("src/Api");
        std::fs::create_dir_all(&api).unwrap();
        for f in ["Api.csproj", "Program.cs"] {
            std::fs::copy(fixture.join("src/Api").join(f), api.join(f)).unwrap();
        }
        std::fs::write(dir.path().join(".gitignore"), ".devstack/\nbin/\nobj/\n").unwrap();
        let port = free_port();
        let project = with_compose.then(|| format!("devstack-it-{}-{port}", std::process::id()));
        let mut toml = String::from("version = 1\n[stack]\nname = \"it\"\n");
        if let Some(p) = &project {
            std::fs::create_dir_all(dir.path().join("deploy")).unwrap();
            std::fs::create_dir_all(dir.path().join("dev")).unwrap();
            std::fs::copy(
                fixture.join("deploy/compose.yml"),
                dir.path().join("deploy/compose.yml"),
            )
            .unwrap();
            // The fixture's overlay pins host port 15432; tests use dynamic ports only.
            let overlay = std::fs::read_to_string(fixture.join("dev/compose.dev.yml"))
                .unwrap()
                .replace("127.0.0.1:15432:5432", "127.0.0.1::5432");
            std::fs::write(dir.path().join("dev/compose.dev.yml"), overlay).unwrap();
            std::fs::write(
                dir.path().join("dev/.env.defaults"),
                "APP_MODE=development\n",
            )
            .unwrap();
            toml.push_str(&format!(
                "[compose.app]\nproject = \"{p}\"\nfiles = [\"deploy/compose.yml\", \"dev/compose.dev.yml\"]\nenv_files = [\"dev/.env.defaults\"]\nservices = [\"db\", \"cache\"]\n"
            ));
        }
        toml.push_str("[services.api]\n");
        if with_compose {
            toml.push_str("compose = { group = \"app\", service = \"api\" }\nrewrite_hosts = [\"db\", \"cache\"]\n");
        }
        toml.push_str(&format!(
            r#"watch = ["src/Api/**"]
build = "dotnet build src/Api/Api.csproj -c Debug -nologo -v q"
output = "src/Api/bin/Debug/net10.0"
run = "dotnet {{artifact}}/Api.dll"
port = {port}
health = {{ http = "/health", timeout = "90s" }}
env = {{ ASPNETCORE_URLS = "http://127.0.0.1:{{port}}" }}
workers = {{ env = {{ Jobs__Enabled = "{{is_main}}" }} }}
after = ["prep"]

# A job before the API, like a schema migrator: it must end as `completed`.
[services.prep]
kind = "job"
run = "git --version"

[commands.ping]
description = "health"
requires = ["api"]
steps = [{{ http = "GET {{service.api.url}}/health" }}]

[commands.reset]
description = "restart the api"
confirm = "Restart?"
steps = [{{ stop = "api" }}, {{ exec = "git --version" }}, {{ start = "api" }}]
"#
        ));
        toml.push_str(if with_compose {
            "
[groups.app]
members = [\"api\", \"compose.app.db\", \"command.ping\"]
"
        } else {
            "
[groups.app]
members = [\"api\", \"command.ping\"]
"
        });
        std::fs::write(dir.path().join("devstack.toml"), toml).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git init");
        Self {
            dir,
            token_home: tempfile::tempdir().unwrap(),
            port,
            project,
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("devstack.toml")
    }

    /// Run the CLI with an isolated token location.
    fn cli(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args).arg("-f").arg(self.file());
        cmd.env("LOCALAPPDATA", self.token_home.path())
            .env("XDG_STATE_HOME", self.token_home.path());
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd.spawn().unwrap();
        if let Some(text) = stdin {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }

    /// `devstack mcp` as an agent would start it.
    fn mcp(&self) -> Mcp {
        let mut child = Command::new(BIN)
            .args(["mcp", "-f"])
            .arg(self.file())
            .env("LOCALAPPDATA", self.token_home.path())
            .env("XDG_STATE_HOME", self.token_home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Mcp {
            stdin: child.stdin.take(),
            child,
            lines: rx,
            next_id: 1,
        }
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args, None);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "devstack {args:?} failed:\n{text}\n--- daemon.log:\n{}",
            self.daemon_log()
        );
        text
    }

    /// Everything that helps when an assertion fails.
    fn diag(&self) -> String {
        let out = |args: &[&str]| {
            let o = self.cli(args, None);
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        };
        format!(
            "--- status:
{}
--- api log:
{}
--- daemon.log:
{}",
            out(&["status"]),
            out(&["logs", "api", "-n", "40"]),
            self.daemon_log()
        )
    }

    fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join(".devstack/daemon.log")).unwrap_or_default()
    }

    fn api(&self, path: &str) -> Option<String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .new_agent();
        agent
            .get(&format!("http://127.0.0.1:{}{path}", self.port))
            .call()
            .ok()?
            .body_mut()
            .read_to_string()
            .ok()
    }

    fn info(&self) -> Value {
        serde_json::from_str(&self.api("/info").expect("api answers /info")).unwrap()
    }

    fn daemon_pid(&self) -> u32 {
        let info: Value = serde_json::from_slice(
            &std::fs::read(self.dir.path().join(".devstack/daemon.json")).unwrap(),
        )
        .unwrap();
        info["pid"].as_u64().unwrap() as u32
    }

    fn artifacts(&self) -> usize {
        std::fs::read_dir(self.dir.path().join(".devstack/artifacts/api"))
            .map(|d| d.count())
            .unwrap_or(0)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = self.cli(&["down", "--infra"], None);
    }
}

struct Mcp {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn request(&mut self, method: &str, params: Value) -> Value {
        use std::io::Write;
        let id = self.next_id;
        self.next_id += 1;
        let msg =
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(120))
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            let v: Value = serde_json::from_str(&line).expect("stdout carries JSON-RPC only");
            if v["id"] == id {
                return v;
            }
        }
    }

    fn notify(&mut self, method: &str) {
        use std::io::Write;
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({ "jsonrpc": "2.0", "method": method })
        )
        .unwrap();
        stdin.flush().unwrap();
    }

    /// (text, isError) of a tool call.
    fn call(&mut self, tool: &str, args: Value) -> (String, bool) {
        let r = self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": args }),
        );
        let res = &r["result"];
        assert!(res.is_object(), "{tool}: {r}");
        (
            res["content"][0]["text"].as_str().unwrap_or("").to_string(),
            res["isError"].as_bool().unwrap_or(false),
        )
    }

    /// Closing stdin ends the session.
    fn close(mut self) {
        drop(self.stdin.take());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("devstack mcp did not end when stdin closed");
    }
}

fn kill_hard(pid: u32) {
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    } else {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

fn wait_for(cond: impl Fn() -> bool, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    cond()
}

fn lifecycle(with_compose: bool) {
    let s = Stack::new(with_compose);

    // up: built, started, healthy; the API sees what devstack gives it.
    s.ok(&["up"]);
    let info = s.info();
    let first_build = info["build"]
        .as_str()
        .expect("DEVSTACK_BUILD_ID")
        .to_string();
    assert_eq!(info["slot"], "main");
    assert_eq!(
        info["workers"], true,
        "workers on in the main slot (ADR-0008)"
    );
    if with_compose {
        let db = s.api("/env/ConnectionStrings__Db").unwrap();
        assert!(
            db.starts_with("Server=127.0.0.1:") && !db.contains("db:5432"),
            "host rewritten (ADR-0014): {db}"
        );
        assert_eq!(
            s.api("/env/Mode").unwrap(),
            "development",
            "value from the dev env file"
        );
    }
    let status = s.ok(&["status"]);
    assert!(status.contains("healthy"), "{status}");
    assert!(
        status
            .lines()
            .any(|l| l.starts_with("prep") && l.contains("completed (0)")),
        "a finished job stays `completed`, it is no crash: {status}"
    );

    // The API refuses requests that are not from the user's own processes (ADR-0019).
    let daemon: Value =
        serde_json::from_slice(&std::fs::read(s.dir.path().join(".devstack/daemon.json")).unwrap())
            .unwrap();
    let base = format!("http://127.0.0.1:{}", daemon["port"]);
    let token = std::fs::read_to_string(s.token_home.path().join("devstack/token")).unwrap();
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();
    assert_eq!(
        agent
            .get(&format!("{base}/v1/stack"))
            .call()
            .unwrap()
            .status(),
        401,
        "no token"
    );
    let foreign_origin = agent
        .post(&format!("{base}/v1/services/api/stop"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Origin", "https://evil.example")
        .send_empty()
        .unwrap();
    assert_eq!(foreign_origin.status(), 403, "foreign Origin");

    // Resource usage (plan §12): the running API is measured, and with
    // compose its containers too.
    let usage = || -> Value {
        let mut r = agent
            .get(&format!("{base}/v1/usage?history=true"))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap();
        serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
    };
    let api_measured = |u: &Value| {
        u["services"].as_array().unwrap().iter().any(|e| {
            e["name"] == "api"
                && e["processes"].as_u64().unwrap_or(0) >= 1
                && e["memory"].as_u64().unwrap_or(0) > 10 * 1024 * 1024
                && e["history"].as_array().is_some_and(|h| !h.is_empty())
        })
    };
    let db_measured = |u: &Value| {
        u["containers"].as_array().unwrap().iter().any(|c| {
            c["group"] == "app" && c["service"] == "db" && c["memory"].as_u64().unwrap_or(0) > 0
        })
    };
    assert!(
        wait_for(
            || {
                let u = usage();
                api_measured(&u) && (!with_compose || db_measured(&u))
            },
            Duration::from_secs(30)
        ),
        "usage not measured: {}
{}",
        usage(),
        s.diag()
    );
    let u = usage();
    assert!(u["lanes"][0]["disk"].is_object(), "disk: {u}");
    let app = u["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "app")
        .unwrap_or_else(|| panic!("groups: {u}"));
    let api_mem = u["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "api")
        .unwrap()["memory"]
        .as_u64()
        .unwrap();
    assert!(
        app["memory"].as_u64().unwrap() >= api_mem,
        "a group sums its members: {u}"
    );
    let top = s.ok(&["top"]);
    assert!(top.contains("api") && top.contains("lane main"), "{top}");

    // The MCP server: a client of the same API, one endpoint per tool.
    let mut m = s.mcp();
    let init = m.request(
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                            "clientInfo": { "name": "it", "version": "0" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "devstack");
    m.notify("notifications/initialized");
    let list = m.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "status",
        "up",
        "restart_service",
        "logs",
        "run_command",
        "usage",
    ] {
        assert!(names.contains(&t), "{t} missing: {names:?}");
    }
    let (text, err) = m.call("status", serde_json::json!({}));
    assert!(
        !err && text.contains("api") && text.contains("healthy"),
        "{text}"
    );
    let (text, err) = m.call("logs", serde_json::json!({ "service": "api", "tail": 5 }));
    assert!(!err && !text.is_empty(), "{text}");
    let (text, err) = m.call("run_command", serde_json::json!({ "command": "reset" }));
    assert!(
        err && text.contains("Restart?"),
        "a confirmation is the user's: {text}"
    );
    let (text, err) = m.call("run_command", serde_json::json!({ "command": "ping" }));
    assert!(!err, "{text}");
    let (text, err) = m.call("usage", serde_json::json!({}));
    assert!(!err && text.contains("lane main"), "{text}");
    let (text, err) = m.call("logs", serde_json::json!({ "service": "../x" }));
    assert!(err, "{text}");
    let unknown = m.request(
        "tools/call",
        serde_json::json!({ "name": "nope", "arguments": {} }),
    );
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    assert!(
        s.api("/health").is_some(),
        "the session changed nothing it was not asked to"
    );
    m.close();
    let rebinding = agent
        .get(&format!("{base}/v1/stack"))
        .header("Authorization", &format!("Bearer {token}"))
        .header("Host", "evil.example")
        .call()
        .unwrap();
    assert_eq!(rebinding.status(), 403, "unexpected Host (DNS rebinding)");

    // Commands: HTTP step; confirmation refused; confirmed.
    assert!(s.ok(&["run", "ping"]).contains("200"));
    assert!(
        !s.cli(&["run", "reset"], Some("n\n")).status.success(),
        "a refused confirmation must not run"
    );
    s.ok(&["run", "reset", "--yes"]);
    assert!(
        s.api("/health").is_some(),
        "api back after the reset
{}",
        s.diag()
    );

    // A broken build never replaces the running instance.
    let program = s.dir.path().join("src/Api/Program.cs");
    let source = std::fs::read_to_string(&program).unwrap();
    std::fs::write(&program, format!("{source}\nthis is not C#\n")).unwrap();
    let out = s.cli(&["restart", "api"], None);
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "restart with a compile error must fail"
    );
    assert!(
        text.contains("src") && text.contains("Program.cs") && !text.contains(".devstack"),
        "errors point at the repository: {text}"
    );
    assert_eq!(
        s.info()["build"],
        first_build.as_str(),
        "the old instance keeps running"
    );

    // Fixed: a new build replaces it.
    std::fs::write(&program, format!("{source}\n// changed\n")).unwrap();
    s.ok(&["restart", "api"]);
    let second = s.info();
    let second_build = second["build"].as_str().unwrap().to_string();
    assert_ne!(second_build, first_build);

    // The daemon dies hard: its services die with it (ADR-0009).
    let api_pid = second["pid"].as_u64().unwrap();
    kill_hard(s.daemon_pid());
    assert!(
        wait_for(|| s.api("/health").is_none(), Duration::from_secs(10)),
        "api {api_pid} survived the daemon"
    );

    // up again: same artifact, no build (ADR-0016).
    let artifacts = s.artifacts();
    s.ok(&["up"]);
    assert_eq!(s.info()["build"], second_build.as_str());
    assert_eq!(s.artifacts(), artifacts, "no new build after a restart");

    // down: nothing left.
    s.ok(&["down", "--infra"]);
    assert!(s.api("/health").is_none());
    if let Some(p) = &s.project {
        let out = Command::new("docker")
            .args([
                "ps",
                "-a",
                "--filter",
                &format!("label=com.docker.compose.project={p}"),
                "-q",
            ])
            .output()
            .unwrap();
        assert!(out.stdout.is_empty(), "containers left behind");
    }
}

#[test]
fn lifecycle_services_only() {
    if !enabled("DEVSTACK_INTEGRATION") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION=1 (needs the .NET 10 SDK)");
        return;
    }
    lifecycle(false);
}

#[test]
fn lifecycle_with_compose() {
    if !enabled("DEVSTACK_INTEGRATION_COMPOSE") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION_COMPOSE=1 (needs the .NET 10 SDK and Docker)");
        return;
    }
    lifecycle(true);
}

/// Without a daemon the MCP tools say so instead of starting anything; only
/// `up` would. Needs neither .NET nor Docker.
#[test]
fn mcp_without_a_daemon() {
    let s = Stack::new(false);
    let mut m = s.mcp();
    let init = m.request(
        "initialize",
        serde_json::json!({ "protocolVersion": "1999-01-01" }),
    );
    assert_eq!(
        init["result"]["protocolVersion"], "2025-06-18",
        "an unknown version is answered with ours: {init}"
    );
    assert_eq!(
        m.request("ping", serde_json::json!({}))["result"],
        serde_json::json!({})
    );
    let (text, err) = m.call("status", serde_json::json!({}));
    assert!(err && text.contains("call `up` first"), "{text}");
    assert!(
        !s.dir.path().join(".devstack/daemon.json").exists(),
        "status must not start a daemon"
    );
    let bad = m.request("no/such", serde_json::json!({}));
    assert_eq!(bad["error"]["code"], -32601, "{bad}");
    m.close();
}
