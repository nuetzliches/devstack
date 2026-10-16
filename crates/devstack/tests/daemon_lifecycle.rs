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
    /// The `kind = "static"` site.
    site_port: u16,
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
        // A built frontend, as `kind = "static"` serves it (the build is a no-op).
        std::fs::create_dir_all(dir.path().join("src/Site/assets")).unwrap();
        std::fs::write(dir.path().join("src/Site/index.html"), "<p>static site</p>").unwrap();
        std::fs::write(
            dir.path().join("src/Site/assets/app-1.js"),
            "console.log(1)",
        )
        .unwrap();
        let port = free_port();
        let site_port = free_port();
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
promote = {{ hold_if_changed = ["src/Api/Schema/**"] }}
after = ["prep"]
hot_reload = {{ run = "dotnet run --project src/Api/Api.csproj --no-launch-profile -nologo" }}

# A job before the API, like a schema migrator: it must end as `completed`.
[services.prep]
kind = "job"
run = "echo run>>prep.count"

# Independent of everything and broken: `up` reports it and starts the rest.
[services.broken]
run = "exit 3"

[services.site]
kind = "static"
build = "git --version"
output = "src/Site"
watch = ["src/Site/**"]
port = {site_port}

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
            site_port,
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
            .env("XDG_STATE_HOME", self.token_home.path())
            .env("DEVSTACK_NO_HUB", "1");
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
            .env("DEVSTACK_NO_HUB", "1")
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

    /// The daemon's diagnostic log and its raw output (a panic lands there).
    fn daemon_log(&self) -> String {
        let read = |f: &str| {
            std::fs::read_to_string(self.dir.path().join(".devstack").join(f)).unwrap_or_default()
        };
        format!(
            "{}\n--- daemon.out:\n{}",
            read("daemon.log"),
            read("daemon.out")
        )
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

    /// The daemon's view of one service (`GET /v1/stack`).
    fn service_view(&self, name: &str) -> Value {
        self.stack_view()["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn stack_view(&self) -> Value {
        let daemon: Value = serde_json::from_slice(
            &std::fs::read(self.dir.path().join(".devstack/daemon.json")).unwrap(),
        )
        .unwrap();
        let token = std::fs::read_to_string(self.token_home.path().join("devstack/token")).unwrap();
        let mut r = ureq::get(&format!("http://127.0.0.1:{}/v1/stack", daemon["port"]))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap();
        serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
    }

    /// The first event of a daemon SSE stream.
    fn sse_first(&self, path: &str) -> String {
        use std::io::Read;
        let daemon: Value = serde_json::from_slice(
            &std::fs::read(self.dir.path().join(".devstack/daemon.json")).unwrap(),
        )
        .unwrap();
        let token = std::fs::read_to_string(self.token_home.path().join("devstack/token")).unwrap();
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        let mut r = agent
            .get(&format!("http://127.0.0.1:{}{path}", daemon["port"]))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap();
        let mut reader = r.body_mut().as_reader();
        let mut text = String::new();
        let mut buf = [0u8; 4096];
        while !text.contains("data:") || !text.contains("\n\n") {
            let n = reader.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        text
    }

    fn prep_runs(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("prep.count"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    fn artifacts(&self) -> usize {
        std::fs::read_dir(self.dir.path().join(".devstack/artifacts/api"))
            .map(|d| d.count())
            .unwrap_or(0)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // The daemon's own account is the only evidence once the temp dir is gone.
            eprintln!("--- daemon.log ---\n{}\n--- end ---", self.daemon_log());
        }
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

    // up: built, started, healthy; the API sees what devstack gives it. One
    // independent service fails: `up` says so and still starts the rest.
    let out = s.cli(&["up"], None);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success() && text.contains("broken"), "{text}");
    assert_eq!(s.prep_runs(), 1, "the job before the API ran once");
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
        // A container's output, stdout and stderr in one, by its service name.
        let logs = s.ok(&["logs", "db"]);
        assert!(
            logs.contains("db ready") && logs.contains("db warming up"),
            "container log: {logs}"
        );
        let only = s.ok(&["logs", "db", "--grep", "WARMING"]);
        assert!(
            only.contains("db warming up") && !only.contains("db ready"),
            "{only}"
        );
    }
    // kind = "static": devstack serves the built files, with the SPA fallback.
    assert!(
        wait_for(
            || get_on(s.site_port, "/").is_some(),
            Duration::from_secs(60)
        ),
        "the static site answers\n{}",
        s.diag()
    );
    assert!(get_on(s.site_port, "/").unwrap().contains("static site"));
    assert!(
        get_on(s.site_port, "/assets/app-1.js")
            .unwrap()
            .contains("console.log")
    );
    assert!(
        get_on(s.site_port, "/customers/42")
            .unwrap()
            .contains("static site"),
        "a client route gets index.html"
    );
    assert!(
        get_on(s.site_port, "/assets/missing.js").is_none(),
        "a missing file is a 404"
    );
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

    assert_eq!(s.prep_runs(), 1, "a failed build runs no job");
    let candidate = &s.service_view("api")["candidate"];
    assert_eq!(
        candidate["state"], "failed",
        "the failed build is visible as a candidate: {candidate}"
    );

    // Fixed: a new build replaces it, and the job it comes after runs again
    // first (a migration that came with the change). Behind the proxy the
    // replacement drops no request (ADR-0010): a poller hits the fixed port
    // throughout.
    std::fs::write(&program, format!("{source}\n// changed\n")).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (stop, port) = (stop.clone(), s.port);
        std::thread::spawn(move || {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .http_status_as_error(false)
                .build()
                .new_agent();
            let (mut ok, mut failed) = (0, Vec::new());
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                match agent.get(&format!("http://127.0.0.1:{port}/health")).call() {
                    Ok(r) if r.status() == 200 => ok += 1,
                    Ok(r) => failed.push(r.status().to_string()),
                    Err(e) => failed.push(e.to_string()),
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            (ok, failed)
        })
    };
    s.ok(&["restart", "api"]);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let (ok, failed) = poller.join().unwrap();
    assert!(
        ok > 10 && failed.is_empty(),
        "{ok} served, failed: {failed:?}"
    );
    assert_eq!(
        s.prep_runs(),
        2,
        "restart reruns the jobs the service comes after"
    );
    let second = s.info();
    let second_build = second["build"].as_str().unwrap().to_string();
    assert_ne!(second_build, first_build);
    assert_eq!(
        second["workers"], true,
        "the relay ends with workers on (ADR-0008)"
    );
    assert!(
        s.service_view("api")["candidate"].is_null(),
        "nothing pending after the switch"
    );
    let tagged = ureq::get(&format!("http://127.0.0.1:{}/health", s.port))
        .call()
        .unwrap()
        .headers()
        .get("x-devstack-build")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        tagged.as_deref(),
        Some(second_build.as_str()),
        "the proxy names the build"
    );

    // A group by its name: stop and start every member service.
    s.ok(&["stop", "app"]);
    assert!(s.api("/health").is_none(), "group stopped");
    s.ok(&["start", "app"]);
    assert!(
        wait_for(|| s.api("/health").is_some(), Duration::from_secs(60)),
        "group started again"
    );
    assert_eq!(s.info()["build"], second_build.as_str(), "no new build");
    let second = s.info();

    // The daemon dies hard: its services die with it (ADR-0009).
    let api_pid = second["pid"].as_u64().unwrap();
    kill_hard(s.daemon_pid());
    assert!(
        wait_for(|| s.api("/health").is_none(), Duration::from_secs(10)),
        "api {api_pid} survived the daemon"
    );

    // up again: same artifact, no build (ADR-0016).
    let artifacts = s.artifacts();
    let up = s.cli(&["up"], None); // `broken` fails again
    assert!(
        s.api("/info").is_some(),
        "api not back after up:\n{}{}\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr),
        s.diag()
    );
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

/// Phase 2 (ADR-0003, ADR-0010, ADR-0013): saved edits become candidates on
/// their own and are swapped in behind the proxy, without a single `restart`.
#[test]
fn edits_become_candidates() {
    if !enabled("DEVSTACK_INTEGRATION") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION=1 (needs the .NET 10 SDK)");
        return;
    }
    let s = Stack::new(false);
    s.cli(&["up"], None); // `broken` fails, the API runs
    let program = s.dir.path().join("src/Api/Program.cs");
    let source = std::fs::read_to_string(&program).unwrap();
    let build = || s.info()["build"].as_str().unwrap().to_string();
    let candidate = || s.service_view("api")["candidate"].clone();
    let wait = |what: &str, cond: &dyn Fn() -> bool| {
        assert!(
            wait_for(cond, Duration::from_secs(120)),
            "{what}\ncandidate: {}\n{}",
            candidate(),
            s.diag()
        );
    };

    // 1. A saved edit goes live, no request fails meanwhile.
    let b0 = build();
    let artifacts = s.artifacts();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (stop, port) = (stop.clone(), s.port);
        std::thread::spawn(move || {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .http_status_as_error(false)
                .build()
                .new_agent();
            let mut failed = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                match agent.get(&format!("http://127.0.0.1:{port}/health")).call() {
                    Ok(r) if r.status() == 200 => {}
                    Ok(r) => failed.push(r.status().to_string()),
                    Err(e) => failed.push(e.to_string()),
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            failed
        })
    };
    std::fs::write(&program, format!("{source}\n// edit 1\n")).unwrap();
    wait("the edit goes live", &|| {
        build() != b0 && candidate().is_null()
    });
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = poller.join().unwrap();
    assert!(
        failed.is_empty(),
        "requests failed during the swap: {failed:?}"
    );
    assert_eq!(s.artifacts(), artifacts + 1);
    assert_eq!(s.info()["workers"], true, "workers on after the relay");

    // 2. A change during the build supersedes it: only the latest is published.
    let b1 = build();
    let artifacts = s.artifacts();
    std::fs::write(&program, format!("{source}\n// edit 2\n")).unwrap();
    wait("a build starts", &|| candidate()["state"] == "building");
    std::fs::write(&program, format!("{source}\n// edit 3\n")).unwrap();
    wait("the latest edit goes live", &|| {
        build() != b1 && candidate().is_null()
    });
    assert_eq!(
        s.artifacts(),
        artifacts + 1,
        "the superseded build published nothing"
    );

    // 3. A broken edit never replaces main; it shows as a failed candidate.
    let b2 = build();
    std::fs::write(&program, format!("{source}\nthis is not C#\n")).unwrap();
    wait("the broken edit fails", &|| {
        candidate()["state"] == "failed"
    });
    assert_eq!(build(), b2, "main keeps running");
    let reason = candidate()["reason"].as_str().unwrap_or("").to_string();
    assert!(reason.contains("Program.cs"), "{reason}");
    std::fs::write(&program, format!("{source}\n// edit 4\n")).unwrap();
    wait("the fix goes live", &|| {
        build() != b2 && candidate().is_null()
    });

    // 4. A schema change waits for an explicit promote (ADR-0013).
    let b3 = build();
    let schema = s.dir.path().join("src/Api/Schema");
    std::fs::create_dir_all(&schema).unwrap();
    std::fs::write(schema.join("v2.txt"), "a new table").unwrap();
    wait("the schema change is held", &|| {
        candidate()["state"] == "held"
    });
    assert_eq!(build(), b3, "held: main unchanged");
    s.ok(&["promote", "api"]);
    assert_ne!(build(), b3, "promoted");
    assert!(candidate().is_null());

    // 5. Rollback: the build before the last replacement, without a build.
    let artifacts = s.artifacts();
    s.ok(&["rollback", "api"]);
    assert_eq!(build(), b3, "back to the previous build");
    assert_eq!(s.artifacts(), artifacts, "no build for a rollback");

    // 6. The daemon does not reload its stack file, but says when it changed.
    let file = s.dir.path().join("devstack.toml");
    let original = std::fs::read_to_string(&file).unwrap();
    let changed = || s.stack_view()["config_changed"] == true;
    assert!(!changed(), "unchanged at start");
    std::fs::write(&file, format!("{original}\n# edited\n")).unwrap();
    wait("a changed stack file is reported", &changed);
    let status = s.cli(&["status"], None);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("stack file changed"),
        "status says so"
    );
    // Same content with CRLF (as a Windows checkout writes it): no change.
    let crlf = original.replace("\r\n", "\n").replace('\n', "\r\n");
    std::fs::write(&file, crlf).unwrap();
    wait("line endings alone are no change", &|| !changed());

    // 7. Every decision above is in the diagnostic log, as JSON lines.
    let log = s.cli(&["logs", "--daemon", "--json", "-n", "5000"], None);
    let log = String::from_utf8_lossy(&log.stdout);
    for needle in [
        "\"event\":\"daemon_start\"",
        "\"event\":\"candidate\"",
        "\"event\":\"request\"",
        "\"path\":\"/v1/services/api/promote\"",
        "\"event\":\"config\"",
    ] {
        assert!(log.contains(needle), "{needle} missing in:\n{log}");
    }
    let readable = s.cli(&["logs", "--daemon", "--grep", "promote"], None);
    let readable = String::from_utf8_lossy(&readable.stdout);
    assert!(
        readable.contains("request") && readable.contains("status=200"),
        "{readable}"
    );

    // 8. Earlier instances stay readable: by build, by time, one by one.
    let out = |args: &[&str]| s.ok(args);
    let list = out(&["logs", "api", "--list"]);
    assert!(list.contains("(running now)"), "{list}");
    assert!(
        list.contains(&b0[..12]) && list.contains("build "),
        "{list}"
    );
    let by_build = out(&["logs", "api", "--build", &b0[..8]]);
    assert!(
        by_build.contains(&format!("── build {b0}")) && by_build.contains("── started"),
        "build log and the instance that ran it:\n{by_build}"
    );
    let since = out(&["logs", "api", "--since", "1h"]);
    assert!(
        since.matches("── started").count() >= 4,
        "every instance of the last hour:\n{since}"
    );
    // The build timeline: every edit above, with what became of it.
    let builds = out(&["builds", "api", "-n", "50"]);
    assert!(
        builds.contains("(running now)")
            && builds.contains("live")
            && builds.contains("superseded"),
        "{builds}"
    );

    // Hot-reload mode: the source tree runs on the same port, edits do not
    // become candidates, and off brings builds back (plan §3.4a).
    s.ok(&["hot-reload", "api", "on"]);
    assert!(
        wait_for(|| s.api("/health").is_some(), Duration::from_secs(120)),
        "api answers in hot-reload mode\n{}",
        s.diag()
    );
    assert_eq!(s.service_view("api")["hot_reload"], true);
    assert!(
        s.ok(&["status"]).contains("hot reload"),
        "status names the mode"
    );
    std::fs::write(&program, format!("{source}\n// edit in hot reload\n")).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(candidate().is_null(), "no candidate in hot-reload mode");
    s.ok(&["hot-reload", "api", "off"]);
    assert!(
        s.service_view("api")["build_id"].is_string()
            && s.service_view("api")["hot_reload"] == false,
        "built again: {}",
        s.service_view("api")
    );

    // The console of one build (what a candidate's panel shows).
    let ev = s.sse_first(&format!(
        "/v1/services/api/logs/stream?tail=5&build={}",
        &b0[..8]
    ));
    assert!(
        ev.contains("\"type\":\"line\""),
        "stream of build {b0}: {ev}"
    );
    let bad = s.cli(&["logs", "api", "--log", "../daemon.log"], None);
    assert!(!bad.status.success(), "only kept logs, no other path");

    s.ok(&["down"]);
}

/// GET on a port of this machine, the body.
fn get_on(port: u16, path: &str) -> Option<String> {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .new_agent()
        .get(&format!("http://127.0.0.1:{port}{path}"))
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()
}

#[test]
fn a_worktree_runs_as_a_lane() {
    if !enabled("DEVSTACK_INTEGRATION") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION=1 (needs the .NET 10 SDK)");
        return;
    }
    let s = Stack::new(false);
    // A dev server (no HTTP proxy): `promote --lane` forwards its port byte
    // for byte. devstack's own file server stands in for `ng serve`.
    let web_port = free_port();
    // Something that may run once per machine (a tray app): main tree only.
    let tray_port = free_port();
    let mut toml = std::fs::read_to_string(s.file()).unwrap();
    toml.push_str(&format!(
        "\n[services.web]\nkind = \"devserver\"\nrun = '\"{BIN}\" serve-static src/Site --port {{port}}'\nport = {web_port}\nprepare = {{ run = \"echo x>>prepared.count\" }}\n"
    ));
    toml.push_str(&format!(
        "\n[services.tray]\nkind = \"tool\"\nlanes = false\nrun = '\"{BIN}\" serve-static src/Site --port {{port}}'\nport = {tray_port}\n"
    ));
    std::fs::write(s.file(), toml).unwrap();
    s.cli(&["up"], None); // `broken` fails, the API runs
    assert!(
        wait_for(
            || get_on(web_port, "/").is_some_and(|b| b.contains("static site")),
            Duration::from_secs(30)
        ),
        "the main tree's dev server answers\n{}",
        s.diag()
    );
    let git = |dir: &Path, args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::write(
        s.dir.path().join(".gitignore"),
        ".devstack/\nprep.count\nprepared.count\n",
    )
    .unwrap();
    // Checkouts as the files are (no CRLF on Windows), so main and worktree hash alike.
    git(s.dir.path(), &["config", "core.autocrlf", "false"]);
    git(s.dir.path(), &["add", "-A"]);
    git(s.dir.path(), &["commit", "-qm", "init"]);
    let wt = s
        .dir
        .path()
        .parent()
        .unwrap()
        .join(format!("wt-{}", s.port));
    git(
        s.dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane-test",
            wt.to_str().unwrap(),
        ],
    );
    let wt_cli = |args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .arg("-f")
            .arg(wt.join("devstack.toml"))
            .env("LOCALAPPDATA", s.token_home.path())
            .env("XDG_STATE_HOME", s.token_home.path())
            .env("DEVSTACK_NO_HUB", "1")
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    let lane = format!("wt-{}", s.port);

    // 1. Every worktree is a lane at once, not started.
    let list = s.ok(&["lane", "list"]);
    let row = list
        .lines()
        .find(|l| l.starts_with(&lane))
        .unwrap_or_else(|| panic!("the worktree is a lane: {list}"));
    assert!(
        row.contains("lane-test") && row.contains("stopped"),
        "{list}"
    );

    // 2. `up` in the worktree starts it: on its own ports, no workers.
    std::fs::write(wt.join("src/Site/index.html"), "<p>lane site</p>").unwrap();
    let up = wt_cli(&["up"]);
    assert!(up.contains(&format!("lane `{lane}`")), "{up}");
    // Its ports are moved by 100 per lane number; a taken range is skipped.
    let list = s.ok(&["lane", "list"]);
    let offset: u16 = list
        .lines()
        .find(|l| l.starts_with(&lane))
        .and_then(|l| l.split_whitespace().nth(2))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("offset in {list}"));
    assert_eq!(offset % 100, 0, "{list}");
    let lane_port = s.port + offset;
    assert!(
        wait_for(
            || get_on(lane_port, "/health").is_some(),
            Duration::from_secs(120)
        ),
        "the lane's api answers on {lane_port}: {:?}\n{up}\n{}\nlane status:\n{}",
        ureq::get(&format!("http://127.0.0.1:{lane_port}/health"))
            .call()
            .map(|r| r.status()),
        s.diag(),
        wt_cli(&["status"])
    );
    let lane_info =
        || -> Value { serde_json::from_str(&get_on(lane_port, "/info").unwrap()).unwrap() };
    assert_eq!(
        lane_info()["workers"],
        false,
        "a lane without the main slot runs no workers"
    );
    assert_eq!(s.info()["workers"], true, "main keeps its workers");
    let status = wt_cli(&["status"]);
    assert!(
        status.contains("api") && status.contains(&lane_port.to_string()),
        "status of the lane: {status}"
    );
    assert!(
        wait_for(
            || get_on(web_port + offset, "/").is_some_and(|b| b.contains("lane site")),
            Duration::from_secs(30)
        ),
        "the lane's dev server answers on its own port"
    );

    // The lane's API takes requests for one service (a path parameter of
    // its own under /v1/lanes/<lane>/v1/...), and a `lanes = false` service
    // runs in the main tree only: there it answers, in the lane it is refused.
    let logs = wt_cli(&["logs", "api"]);
    assert!(!logs.contains("path arguments"), "{logs}");
    assert!(
        get_on(tray_port, "/").is_some(),
        "the main tree runs the tray"
    );
    let refused = wt_cli(&["start", "tray"]);
    assert!(refused.contains("main working tree only"), "{refused}");
    assert!(
        get_on(tray_port + offset, "/").is_none(),
        "no tray in the lane"
    );

    // 3. An edit in the worktree reaches the lane only.
    let main_build = s.info()["build"].as_str().unwrap().to_string();
    let lane_build = lane_info()["build"].as_str().unwrap().to_string();
    assert_eq!(
        lane_build, main_build,
        "same sources, same build id: the lane started from the main tree's artifact"
    );
    let program = wt.join("src/Api/Program.cs");
    let source = std::fs::read_to_string(&program).unwrap();
    std::fs::write(&program, format!("{source}\n// lane edit\n")).unwrap();
    assert!(
        wait_for(
            || lane_info()["build"].as_str() != Some(lane_build.as_str()),
            Duration::from_secs(120)
        ),
        "the lane built the edit\n{}",
        s.diag()
    );
    assert_eq!(s.info()["build"], main_build.as_str(), "main is untouched");

    // 4. promote --lane: the main ports answer from the lane, the dev server's
    //    through a TCP forward, and the workers move with them. The static
    //    site was stopped in the main tree: it stays stopped when the main
    //    slot comes back.
    s.ok(&["stop", "site"]);
    let lane_build = lane_info()["build"].as_str().unwrap().to_string();
    s.ok(&["promote", "--lane", &lane]);
    assert!(
        wait_for(
            || s.api("/info").is_some_and(|i| i.contains(&lane_build)),
            Duration::from_secs(60)
        ),
        "the main port serves the lane's build"
    );
    assert!(
        wait_for(
            || get_on(web_port, "/").is_some_and(|b| b.contains("lane site")),
            Duration::from_secs(30)
        ),
        "the dev server's main port forwards to the lane\n{}",
        s.diag()
    );
    assert!(
        wait_for(|| lane_info()["workers"] == true, Duration::from_secs(60)),
        "the lane runs the workers now"
    );
    s.ok(&["promote", "--lane", "main"]);
    assert!(
        wait_for(
            || s.api("/info").is_some_and(|i| i.contains(&main_build)),
            Duration::from_secs(120)
        ),
        "the main tree serves again\n{}",
        s.diag()
    );
    assert!(
        wait_for(
            || get_on(web_port, "/").is_some_and(|b| b.contains("static site")),
            Duration::from_secs(30)
        ),
        "the main tree's dev server runs on its port again\n{}",
        s.diag()
    );
    assert!(wait_for(
        || s.info()["workers"] == true,
        Duration::from_secs(60)
    ));
    assert_eq!(lane_info()["workers"], false);
    assert!(
        get_on(s.site_port, "/").is_none(),
        "what was stopped before stays stopped\n{}",
        s.ok(&["status"])
    );

    // 5. The lane runs from a copy of its worktree: `prepare` ran there, an
    //    edit reaches its dev server within moments, and nothing runs in the
    //    worktree, so git removes it while the lane runs; the lane goes with it.
    let copy = s
        .dir
        .path()
        .join(".devstack")
        .join(format!("lane-{lane}"))
        .join("lanes/main/src");
    assert!(
        copy.join("prepared.count").is_file(),
        "prepare ran in the lane's copy"
    );
    assert!(
        !wt.join("prepared.count").exists(),
        "and never in the worktree"
    );
    std::fs::write(wt.join("src/Site/index.html"), "<p>lane site 2</p>").unwrap();
    assert!(
        wait_for(
            || get_on(web_port + offset, "/").is_some_and(|b| b.contains("lane site 2")),
            Duration::from_secs(15)
        ),
        "the copy follows the worktree"
    );
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "lane edit"]);
    git(s.dir.path(), &["worktree", "remove", wt.to_str().unwrap()]);
    assert!(!wt.exists(), "git removed the worktree of a running lane");
    assert!(
        wait_for(
            || get_on(lane_port, "/health").is_none() && get_on(web_port + offset, "/").is_none(),
            Duration::from_secs(15)
        ),
        "the lane stopped with its worktree\n{}",
        s.diag()
    );
    assert!(!s.ok(&["lane", "list"]).contains(&lane));
    assert!(s.api("/health").is_some(), "main still runs");

    // 6. A worktree added with git is a lane at once: the daemon notices it
    //    without being asked.
    let extra_name = format!("extra-{}", s.port);
    let extra = s.dir.path().parent().unwrap().join(&extra_name);
    git(
        s.dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "extra",
            extra.to_str().unwrap(),
        ],
    );
    let log = s.dir.path().join(".devstack").join("daemon.log");
    assert!(
        wait_for(
            || std::fs::read_to_string(&log).is_ok_and(|t| t
                .lines()
                .any(|l| l.contains("lane_found") && l.contains(&extra_name))),
            Duration::from_secs(10)
        ),
        "the new worktree is found without a request"
    );
    assert!(s.ok(&["lane", "list"]).contains(&extra_name));

    s.ok(&["down"]);
    for dir in [&wt, &extra] {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(dir)
            .current_dir(s.dir.path())
            .status();
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// `docker compose -p <project> ps` as (service, name, published host ports).
fn compose_ps(project: &str) -> Vec<(String, String, Vec<u16>)> {
    let out = Command::new("docker")
        .args(["compose", "-p", project, "ps", "--all", "--format", "json"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|c| {
            let ports = c["Publishers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["PublishedPort"].as_u64())
                .filter(|p| *p != 0)
                .map(|p| p as u16)
                .collect();
            (
                c["Service"].as_str().unwrap_or("").to_string(),
                c["Name"].as_str().unwrap_or("").to_string(),
                ports,
            )
        })
        .collect()
}

fn started_at(container: &str) -> String {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.StartedAt}}", container])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// ADR-0030: a lane runs its own Compose project, its host ports moved like
/// its services', without the fixed container names of the main tree; a
/// `shared` service is the main tree's. Restarting a container of the main
/// tree leaves the lane's alone; `lane stop` and removing the worktree take
/// the lane's containers away.
#[test]
fn a_lane_has_its_own_containers() {
    if !enabled("DEVSTACK_INTEGRATION_COMPOSE") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION_COMPOSE=1 (needs the .NET 10 SDK and Docker)");
        return;
    }
    let s = Stack::new(true);
    let project = s.project.clone().unwrap();
    // A fixed host port and a fixed container name, as deployment files have them.
    let db_port = free_port();
    let main_db = format!("it-db-{db_port}");
    let ov = s.dir.path().join("dev/compose.dev.yml");
    let text = std::fs::read_to_string(&ov).unwrap().replace(
        "  db:\n    ports: [\"127.0.0.1::5432\"]",
        &format!("  db:\n    container_name: {main_db}\n    ports: [\"127.0.0.1:{db_port}:5432\"]"),
    );
    assert!(text.contains(&main_db), "overlay changed: {text}");
    std::fs::write(&ov, text).unwrap();
    let toml = std::fs::read_to_string(s.file()).unwrap().replace(
        "services = [\"db\", \"cache\"]\n",
        "services = [\"db\", \"cache\"]\nshared = [\"cache\"]\n",
    );
    std::fs::write(s.file(), toml).unwrap();
    s.cli(&["up"], None); // `broken` fails, the rest runs

    let git = |dir: &Path, args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::write(
        s.dir.path().join(".gitignore"),
        ".devstack/\nprep.count\nbin/\nobj/\n",
    )
    .unwrap();
    git(s.dir.path(), &["config", "core.autocrlf", "false"]);
    git(s.dir.path(), &["add", "-A"]);
    git(s.dir.path(), &["commit", "-qm", "init"]);
    let wt = s
        .dir
        .path()
        .parent()
        .unwrap()
        .join(format!("wt-{}", s.port));
    git(
        s.dir.path(),
        &["worktree", "add", "-q", "-b", "iso", wt.to_str().unwrap()],
    );
    let lane = format!("wt-{}", s.port);
    let lane_project = format!("{project}-{lane}");
    let up = Command::new(BIN)
        .args(["up", "-f"])
        .arg(wt.join("devstack.toml"))
        .env("LOCALAPPDATA", s.token_home.path())
        .env("XDG_STATE_HOME", s.token_home.path())
        .env("DEVSTACK_NO_HUB", "1")
        .output()
        .unwrap();
    let up = format!(
        "{}{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    let offset: u16 = s
        .ok(&["lane", "list"])
        .lines()
        .find(|l| l.starts_with(&lane))
        .and_then(|l| l.split_whitespace().nth(2))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("lane offset\n{up}"));

    // Its own db, on the moved port, under a name of its own; no cache of its own.
    let ours = compose_ps(&lane_project);
    let db = ours
        .iter()
        .find(|c| c.0 == "db")
        .unwrap_or_else(|| panic!("the lane runs its own db: {ours:?}\n{up}"));
    assert_eq!(db.2, vec![db_port + offset], "{ours:?}");
    assert_ne!(db.1, main_db, "no fixed container name in a lane");
    assert!(
        !ours.iter().any(|c| c.0 == "cache"),
        "cache is shared: {ours:?}"
    );
    let main = compose_ps(&project);
    assert!(
        main.iter().any(|c| c.1 == main_db && c.2 == vec![db_port]),
        "{main:?}"
    );

    // Restarting the main tree's db does not touch the lane's.
    let before = started_at(&db.1);
    let st = Command::new("docker")
        .args(["restart", &main_db])
        .output()
        .unwrap();
    assert!(st.status.success());
    assert_eq!(started_at(&db.1), before, "the lane's db was restarted too");

    // `lane stop` gives the containers back; a removed worktree takes the rest.
    s.ok(&["lane", "stop", &lane]);
    assert!(
        compose_ps(&lane_project).is_empty(),
        "containers after lane stop"
    );
    git(
        s.dir.path(),
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    assert!(
        wait_for(
            || !s.ok(&["lane", "list"]).contains(&lane),
            Duration::from_secs(15)
        ),
        "lane gone with its worktree"
    );
    assert!(compose_ps(&lane_project).is_empty());
    let _ = Command::new("docker")
        .args(["compose", "-p", &lane_project, "down", "--volumes"])
        .output();
}
