//! A lane on another machine of the pool, end to end through the CLI
//! (ADR-0025, ADR-0029, ADR-0031): a coordinator, a node agent and a
//! requester on one Linux host. First without the daemon (`node run-lane`),
//! then as a developer runs it: `devstack up --on` in a worktree, the lane's
//! API on its forwarded local port, `devstack down`. Each time the node is
//! left with nothing.
//!
//! Opt-in: it needs Docker and a static Linux build of devstack for the lane
//! container (the release's kind), named by `DEVSTACK_LANE_BIN`:
//!   DEVSTACK_INTEGRATION_NODE=1 DEVSTACK_LANE_BIN=<musl build>
//! and `-- --include-ignored` (the test is `#[ignore]`d by default).

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_devstack");

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `devstack <args>` as the member in `pool` (or none), output checked.
fn run(pool: Option<&Path>, args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args);
    if let Some(p) = pool {
        c.env("DEVSTACK_POOL_DIR", p);
    }
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "devstack {}: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn join(url: &str, fp: &str, coord: &Path, pool: &Path, name: &str) {
    run(
        Some(pool),
        &[
            "join",
            url,
            "--fingerprint",
            fp,
            "--name",
            name,
            "--no-wait",
        ],
    );
    run(
        None,
        &[
            "node",
            "members",
            "approve",
            name,
            "--dir",
            &coord.to_string_lossy(),
        ],
    );
    run(Some(pool), &["join", url, "--fingerprint", fp]);
}

#[test]
#[ignore = "needs Linux, Docker and a static build: DEVSTACK_INTEGRATION_NODE=1, DEVSTACK_LANE_BIN, --include-ignored"]
fn a_lane_runs_on_a_node_of_the_pool() {
    if std::env::var("DEVSTACK_INTEGRATION_NODE").as_deref() != Ok("1") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION_NODE=1 (Linux, Docker, DEVSTACK_LANE_BIN)");
        return;
    }
    let lane_bin = std::env::var("DEVSTACK_LANE_BIN").expect("DEVSTACK_LANE_BIN");
    let (coord, node, me) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let c = coord.path().to_string_lossy().into_owned();
    run(
        None,
        &["node", "init", "--address", "127.0.0.1", "--dir", &c],
    );
    let fp = text(&run(None, &["node", "fingerprint", "--dir", &c]))
        .trim()
        .to_string();
    let cport = free_port();
    let _coordinator = Kill(
        Command::new(BIN)
            .args([
                "node",
                "serve",
                "--listen",
                &format!("127.0.0.1:{cport}"),
                "--dir",
                &c,
            ])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let url = format!("https://127.0.0.1:{cport}");
    let until = Instant::now() + Duration::from_secs(20);
    while std::net::TcpStream::connect(("127.0.0.1", cport)).is_err() {
        assert!(Instant::now() < until, "the coordinator does not listen");
        std::thread::sleep(Duration::from_millis(100));
    }
    join(&url, &fp, coord.path(), node.path(), "ci-node");
    join(&url, &fp, coord.path(), me.path(), "ci-dev");
    // Only a node an admin granted runs the pool's lanes.
    run(
        None,
        &["node", "members", "node", "ci-node", "on", "--dir", &c],
    );

    let nport = free_port();
    let _agent = Kill(
        Command::new(BIN)
            .args([
                "node",
                "agent",
                "--listen",
                &format!("127.0.0.1:{nport}"),
                "--max-cpus",
                "2",
                "--max-memory-gib",
                "2",
                // A server: a lane there may take the main slot (ADR-0025
                // point 4).
                "--dedicated",
            ])
            .env("DEVSTACK_POOL_DIR", node.path())
            .env("DEVSTACK_LANE_BIN", &lane_bin)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let pool = text(&run(Some(me.path()), &["pool"]));
        if pool
            .lines()
            .any(|l| l.contains("ci-node") && l.contains("online"))
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "the node never came online:\n{pool}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture-stack/devstack.toml");
    let out = run(
        Some(me.path()),
        &[
            "-f",
            &fixture.to_string_lossy(),
            "node",
            "run-lane",
            "--on",
            "ci-node",
            "--lane",
            "ci",
        ],
    );
    let log = text(&out);
    assert!(log.contains("api=healthy"), "{log}");

    // Released: its container and its Compose project are gone.
    node_is_clear();

    // As a developer runs it: a worktree of a repository with the fixture.
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let main = repo.path().join("main");
    let wt = repo.path().join("wt");
    copy_fixture(&main);
    slow_db(&main);
    with_prepare(&main);
    git(&main, &["init", "-q"]);
    git(&main, &["add", "-A"]);
    git(
        &main,
        &[
            "-c",
            "user.email=ci@example.invalid",
            "-c",
            "user.name=ci",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            &wt.to_string_lossy(),
            "-b",
            "ci-remote",
        ],
    );
    let cli = |dir: &Path, args: &[&str]| {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("DEVSTACK_POOL_DIR", me.path())
            .env("XDG_STATE_HOME", state.path())
            .env("DEVSTACK_NO_HUB", "1")
            .env("DEVSTACK_NO_UPDATE_CHECK", "1")
            .output()
            .unwrap();
        let both = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "devstack {}: {both}", args.join(" "));
        both
    };
    // The daemon answers 202 once leased; `up` follows the start on the
    // node step by step, which ends when the lane's daemon answers there,
    // then the lane's services as they start.
    let mut child = Command::new(BIN)
        .args(["up", "--on", "ci-node"])
        .current_dir(&wt)
        .env("DEVSTACK_POOL_DIR", me.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVSTACK_NO_HUB", "1")
        .env("DEVSTACK_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |p: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = p {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let api = |path: &str| api_get(&main, state.path(), path);
    // The lane runs there once its daemon answers (#164): its containers
    // come up after, under it. The fixture's db here is healthy 20 s after
    // it starts (`slow_db`), so the daemon answers well before.
    let until = Instant::now() + Duration::from_secs(900);
    loop {
        let lane = api("/v1/lanes")
            .and_then(|v| v.as_array()?.iter().find(|l| l["name"] == "wt").cloned());
        if lane
            .as_ref()
            .is_some_and(|l| l["active"] == true && l["moving"].is_null())
        {
            break;
        }
        if let Some(st) = child.try_wait().unwrap() {
            panic!(
                "`up --on` ended ({st}) before the lane ran:\n{}{}",
                out.join().unwrap(),
                err.join().unwrap()
            );
        }
        assert!(Instant::now() < until, "the lane never ran: {lane:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
    let health = lane_db("{{.State.Health.Status}}");
    assert_ne!(
        health, "healthy",
        "the lane's daemon answered only once its db was healthy"
    );
    // Its services start only once its infrastructure is up, as a local
    // lane's: `api` leaves `stopped` with the db healthy.
    let until = Instant::now() + Duration::from_secs(600);
    loop {
        let api_state = api("/v1/lanes/wt/v1/stack").and_then(|v| {
            v["services"]
                .as_array()?
                .iter()
                .find(|s| s["name"] == "api")
                .and_then(|s| s["status"]["state"].as_str().map(str::to_string))
        });
        if let Some(st) = api_state.filter(|s| s != "stopped") {
            assert_eq!(
                lane_db("{{.State.Health.Status}}"),
                "healthy",
                "api is {st} before the db is healthy"
            );
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none() && Instant::now() < until,
            "api never started"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    // The lane's own daemon keeps its lines for this daemon to send with
    // its own telemetry (ADR-0037 addendum, #287); taken once. This test's
    // daemon sends no telemetry, so nothing took them before.
    let (st, body) = api_request(
        &main,
        state.path(),
        "POST",
        "/v1/lanes/wt/v1/telemetry/take",
    )
    .expect("the lane answers");
    assert_eq!(st, 200, "{body}");
    let taken: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        taken["lines"]
            .as_array()
            .is_some_and(|l| l.iter().any(|l| l["event"] == "daemon_start")),
        "{body}"
    );
    let (_, again) = api_request(
        &main,
        state.path(),
        "POST",
        "/v1/lanes/wt/v1/telemetry/take",
    )
    .expect("the lane answers");
    assert!(!again.contains("daemon_start"), "taken once: {again}");

    // What `prepare` made is linked into the worktree here, from a copy on
    // this machine (ADR-0063), and a git hook there finds it (#277).
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let links = api("/v1/lanes")
            .and_then(|v| v.as_array()?.iter().find(|l| l["name"] == "wt").cloned())
            .map(|l| l["links"].clone());
        if links
            .as_ref()
            .and_then(|l| l.as_array()?.first().cloned())
            .is_some_and(|l| l["state"].as_str().is_some_and(|s| s.starts_with("linked")))
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "the worktree was not linked: {links:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(
        std::fs::read_to_string(wt.join("deps/made.txt"))
            .unwrap()
            .trim(),
        "made"
    );
    let hook = main.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\ntest -f deps/made.txt\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(wt.join("note.txt"), "hooked").unwrap();
    git(&wt, &["add", "note.txt"]);
    git(
        &wt,
        &[
            "-c",
            "user.email=ci@example.invalid",
            "-c",
            "user.name=ci",
            "commit",
            "-qm",
            "a commit through the hook",
        ],
    );
    assert!(
        cli(&wt, &["status"]).contains("deps linked"),
        "status names the link"
    );
    // The node took what the lane's `prepare` made into the member's store
    // (ADR-0062 points 3, 4).
    let until = Instant::now() + Duration::from_secs(120);
    while store_entries(node.path()) == 0 {
        assert!(Instant::now() < until, "the node's store took nothing in");
        std::thread::sleep(Duration::from_secs(1));
    }

    let status = child.wait().unwrap();
    let up = format!("{}{}", out.join().unwrap(), err.join().unwrap());
    assert!(status.success(), "devstack up --on ci-node: {up}");
    assert!(up.contains("on ci-node"), "{up}");
    assert!(up.contains("[3/3] on ci-node: "), "{up}");
    assert!(up.contains("healthy"), "{up}");
    assert!(
        !up.contains("waiting until healthy"),
        "the start waits for no container: {up}"
    );
    // Each phase of the start is timed (B1).
    let log =
        std::fs::read_to_string(main.join(".devstack").join("daemon.log")).unwrap_or_default();
    for phase in [
        "\"lease\"",
        "\"sync\"",
        "images (pull, build) and containers",
        "the lane's daemon",
    ] {
        assert!(
            log.lines()
                .any(|l| l.contains("remote_lane_phase") && l.contains(phase)),
            "no remote_lane_phase for {phase}:\n{log}"
        );
    }
    let list = cli(&main, &["lane", "list"]);
    let line = list
        .lines()
        .find(|l| l.starts_with("wt"))
        .unwrap_or_else(|| panic!("{list}"));
    assert!(line.contains("ci-node"), "{list}");
    let offset: u16 = line
        .split_whitespace()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{list}"));

    // The lane's API on its port here, forwarded to the node (ADR-0029 point 4).
    let port = 5080 + offset;
    let body = http_get(port, "/health");
    assert!(body.contains("ok"), "GET /health on {port}: {body}");

    // The lane's containers, which its daemon cannot reach (no Docker in the
    // lane container): their output and what they use come from the node
    // (#101).
    let logs = cli(&wt, &["logs", "db", "-n", "20"]);
    assert!(logs.contains("db ready"), "{logs}");
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let top = cli(&wt, &["top"]);
        let db = top.lines().find(|l| l.trim_start().starts_with("app/db"));
        if db.is_some_and(|l| l.contains("iB")) {
            break;
        }
        assert!(
            Instant::now() < until,
            "no load measured for app/db:\n{top}"
        );
        std::thread::sleep(Duration::from_secs(3));
    }

    // They are restarted through the node too, the lane's own (its project
    // `devstack-fixture-<slot>`), never the main tree's (#128): one by its
    // name, then the group's container members with its services.
    let before = lane_db_started();
    cli(&wt, &["restart", "app.db"]);
    let after = lane_db_started();
    assert_ne!(before, after, "app.db restarted on the node");
    cli(&wt, &["restart", "app"]);
    assert_ne!(after, lane_db_started(), "the group restarted its db");

    // The main slot to the lane on the node: the fixed port answers from
    // there, and comes back to the main tree (ADR-0025 point 4).
    cli(&main, &["promote", "--lane", "wt"]);
    let body = http_get(5080, "/health");
    assert!(body.contains("ok"), "GET /health on the main port: {body}");
    let list = cli(&main, &["lane", "list"]);
    assert!(
        list.lines()
            .any(|l| l.starts_with("wt") && l.contains("ci-node")),
        "{list}"
    );
    cli(&main, &["promote", "--lane", "main"]);

    // A restart carries the lane over (ADR-0052): the next daemon renews its
    // lease, its containers on the node run on untouched, its port here
    // answers again, and the main slot it held goes back to it.
    cli(&main, &["promote", "--lane", "wt"]);
    let db_before = lane_db_started();
    let (st, _) = api_request(&main, state.path(), "POST", "/v1/shutdown?restart=true")
        .expect("the daemon answers the restart");
    assert_eq!(st, 202);
    let until = Instant::now() + Duration::from_secs(120);
    while main.join(".devstack/daemon.json").exists() {
        assert!(Instant::now() < until, "the daemon did not stop");
        std::thread::sleep(Duration::from_millis(200));
    }
    cli(&main, &["up"]);
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let lane = api("/v1/lanes")
            .and_then(|v| v.as_array()?.iter().find(|l| l["name"] == "wt").cloned());
        if lane
            .as_ref()
            .is_some_and(|l| l["active"] == true && l["node"] == "ci-node" && l["moving"].is_null())
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "the lane was not carried over: {lane:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(
        lane_db_started(),
        db_before,
        "the lane's containers on the node ran on"
    );
    let until = Instant::now() + Duration::from_secs(120);
    let log = loop {
        let log =
            std::fs::read_to_string(main.join(".devstack").join("daemon.log")).unwrap_or_default();
        if log.contains("\"main_slot_carried\"") {
            break log;
        }
        assert!(
            Instant::now() < until && !log.contains("\"main_slot_not_carried\""),
            "the main slot did not go back to the lane:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(log.contains("\"lane_carried\""), "{log}");
    let body = http_get(port, "/health");
    assert!(
        body.contains("ok"),
        "GET /health on {port} after the restart: {body}"
    );
    let body = http_get(5080, "/health");
    assert!(
        body.contains("ok"),
        "GET /health on the main port after the restart: {body}"
    );
    cli(&main, &["promote", "--lane", "main"]);

    // `down` in the main tree stops the main tree only: the lane on the
    // node runs on, and answers on its port here.
    let down = cli(&main, &["down"]);
    assert!(down.contains("lanes keep running"), "{down}");
    let body = http_get(port, "/health");
    assert!(
        body.contains("ok"),
        "GET /health on {port} after `down`: {body}"
    );

    // A daemon that ends without stopping the lane (killed, as by a reboot):
    // the next one says with the lane that it is gone, and its lease goes
    // (ADR-0052 point 6).
    let info: serde_json::Value = serde_json::from_slice(
        &std::fs::read(main.join(".devstack").join("daemon.json")).expect("daemon.json"),
    )
    .expect("daemon.json is JSON");
    let pid = info["pid"].as_u64().expect("the daemon's pid");
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    let until = Instant::now() + Duration::from_secs(30);
    while Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
    {
        assert!(Instant::now() < until, "the daemon did not die");
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(main.join(".devstack").join("daemon.json"));
    cli(&main, &["up"]);
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let lane = api("/v1/lanes")
            .and_then(|v| v.as_array()?.iter().find(|l| l["name"] == "wt").cloned());
        if let Some(l) = &lane
            && let Some(e) = l["moving"]["error"].as_str()
        {
            assert!(e.contains("ended without stopping it"), "{l}");
            assert_ne!(l["active"], true, "{l}");
            break;
        }
        assert!(
            Instant::now() < until,
            "the lane nobody followed was not said: {lane:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    let log =
        std::fs::read_to_string(main.join(".devstack").join("daemon.log")).unwrap_or_default();
    assert!(log.contains("\"remote_lane_unfollowed\""), "{log}");
    node_is_clear();

    // Its data thrown away, the lane starts afresh on the node: its build's
    // `prepare` comes from the member's store there (ADR-0062).
    cli(&main, &["lane", "reset", "wt"]);
    cli(&wt, &["up", "--on", "ci-node"]);
    let (st, body) = api_request(
        &main,
        state.path(),
        "POST",
        "/v1/lanes/wt/v1/telemetry/take",
    )
    .expect("the lane answers");
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("prepare_store_hit"), "{body}");

    // `down --all`: the daemon ends, and the lane on the node goes with it
    // (ADR-0025 point 7).
    cli(&main, &["down", "--all"]);
    node_is_clear();
}

/// No lane container is left on the node, and no container of a lane's
/// Compose project (`devstack-fixture-<slot>`, the slot 24 hex digits).
fn node_is_clear() {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let ps = Command::new("docker")
            .args(["ps", "-aq", "--filter", "label=devstack.lease"])
            .output()
            .unwrap();
        let projects = Command::new("docker")
            .args([
                "ps",
                "-a",
                "--filter",
                "label=com.docker.compose.project",
                "--format",
                "{{.Label \"com.docker.compose.project\"}}",
            ])
            .output()
            .unwrap();
        let lanes: Vec<String> = String::from_utf8_lossy(&projects.stdout)
            .lines()
            .filter(|p| {
                p.strip_prefix("devstack-fixture-")
                    .is_some_and(|s| s.len() == 24 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            })
            .map(str::to_string)
            .collect();
        if ps.stdout.is_empty() && lanes.is_empty() {
            return;
        }
        assert!(
            Instant::now() < until,
            "a lane container stays on the node: {lanes:?}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// When the lane's `db` container on the node last started: the one of a
/// project `devstack-fixture-<slot>`, not the main tree's.
fn lane_db_started() -> String {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            "label=com.docker.compose.service=db",
            "--format",
            "{{.Names}}\t{{.Label \"com.docker.compose.project\"}}",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let name = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .find(|(_, p)| p.starts_with("devstack-fixture-"))
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| panic!("no lane db container:\n{text}"));
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.StartedAt}}", &name])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `docker inspect -f <format>` of the lane's `db` container on the node
/// (of a project `devstack-fixture-<slot>`, running or not); empty while
/// there is none.
fn lane_db(format: &str) -> String {
    let out = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "label=com.docker.compose.service=db",
            "--format",
            "{{.Names}}\t{{.Label \"com.docker.compose.project\"}}",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let Some(name) = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .find(|(_, p)| p.starts_with("devstack-fixture-"))
        .map(|(n, _)| n.to_string())
    else {
        return String::new();
    };
    let out = Command::new("docker")
        .args(["inspect", "-f", format, &name])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The fixture's db made slow: healthy 20 s after it starts, so a lane's
/// daemon that waited for its containers would answer after that (#164).
fn slow_db(main: &Path) {
    std::fs::write(
        main.join("dev/compose.dev.yml"),
        r#"services:
  api:
    profiles: [container-app]
    depends_on: !override
      db: { condition: service_started, required: false }
      cache: { condition: service_started, required: false }
  db:
    ports: ["127.0.0.1:15432:5432"]
    command: ["sh", "-c", "echo db ready; (sleep 20; touch /tmp/healthy) & trap 'exit 0' TERM; while true; do sleep 1; done"]
    healthcheck:
      test: ["CMD", "test", "-f", "/tmp/healthy"]
      interval: 1s
      start_period: 120s
      start_interval: 1s
  cache:
    ports: ["127.0.0.1::6379"]
"#,
    )
    .unwrap();
}

/// The fixture's API gets a `prepare` whose outputs are kept in the store
/// and linked into a worktree (ADR-0062, ADR-0063); git ignores the link.
fn with_prepare(main: &Path) {
    let file = main.join("devstack.toml");
    let toml = std::fs::read_to_string(&file).unwrap();
    let port = "port          = 5080\n";
    assert!(toml.contains(port), "{toml}");
    let toml = toml.replacen(
        port,
        &format!(
            "{port}prepare       = {{ run = \"mkdir -p deps && echo made > deps/made.txt\", cwd = \".\", inputs = [\"src/Api/Api.csproj\"], outputs = [\"deps\"], link = [\"deps\"] }}\n"
        ),
        1,
    );
    std::fs::write(&file, toml).unwrap();
    let ignore = main.join(".gitignore");
    let mut text = std::fs::read_to_string(&ignore).unwrap();
    text.push_str("deps\n");
    std::fs::write(&ignore, text).unwrap();
}

/// The entries of every member store the node agent keeps under `dir`.
fn store_entries(dir: &Path) -> usize {
    fn count(p: &Path, in_stores: bool) -> usize {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let path = e.path();
                if in_stores && path.join(".devstack-store.json").is_file() {
                    1
                } else if path.is_dir() && !path.is_symlink() {
                    count(&path, in_stores || e.file_name() == "stores")
                } else {
                    0
                }
            })
            .sum()
    }
    count(dir, false)
}

/// `GET <path>` of the requester's daemon (the main tree's), as its JSON;
/// `None` while it does not answer.
fn api_get(main: &Path, state: &Path, path: &str) -> Option<serde_json::Value> {
    let (status, body) = api_request(main, state, "GET", path)?;
    if status != 200 {
        return None;
    }
    serde_json::from_str(&body).ok()
}

/// `<method> <path>` of the requester's daemon, without a body: its status
/// and body; `None` while it does not answer.
fn api_request(main: &Path, state: &Path, method: &str, path: &str) -> Option<(u16, String)> {
    use std::io::{Read, Write};
    let info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(main.join(".devstack/daemon.json")).ok()?).ok()?;
    let port = info["port"].as_u64()? as u16;
    let token = std::fs::read_to_string(state.join("devstack/token")).ok()?;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    // HTTP/1.0: the body comes whole, not in chunks.
    write!(
        s,
        "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\n\r\n",
        token.trim()
    )
    .ok()?;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n")?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, body.to_string()))
}

/// The fixture stack's files (not its local build output) into `to`.
fn copy_fixture(to: &Path) {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture-stack");
    for rel in [
        "devstack.toml",
        "deploy/compose.yml",
        "dev/compose.dev.yml",
        "dev/.env.defaults",
        "src/Api/Api.csproj",
        "src/Api/Program.cs",
    ] {
        let target = to.join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(from.join(rel), &target).unwrap();
    }
    std::fs::write(to.join(".gitignore"), ".devstack/\nbin/\nobj/\n").unwrap();
}

/// A plain HTTP GET on loopback; the body.
fn http_get(port: u16, path: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port))
        .unwrap_or_else(|e| panic!("connect to {port}: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}
