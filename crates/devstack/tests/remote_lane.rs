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
    // node step by step and ends when the lane runs there.
    let up = cli(&wt, &["up", "--on", "ci-node"]);
    assert!(up.contains("on ci-node"), "{up}");
    assert!(up.contains("[3/3] on ci-node: "), "{up}");
    assert!(up.contains("healthy"), "{up}");
    // Each phase of the start is timed (B1).
    let log =
        std::fs::read_to_string(main.join(".devstack").join("daemon.log")).unwrap_or_default();
    for phase in [
        "\"lease\"",
        "\"sync\"",
        "waiting until healthy",
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

    // The daemon ends: the lane on the node goes with it (ADR-0025 point 7).
    cli(&main, &["down"]);
    node_is_clear();
}

/// No lane container is left on the node.
fn node_is_clear() {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let ps = Command::new("docker")
            .args(["ps", "-aq", "--filter", "label=devstack.lease"])
            .output()
            .unwrap();
        if ps.stdout.is_empty() {
            return;
        }
        assert!(Instant::now() < until, "a lane container stays on the node");
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
