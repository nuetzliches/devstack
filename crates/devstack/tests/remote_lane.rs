//! A lane on another machine of the pool, end to end through the CLI
//! (ADR-0025, ADR-0031): a coordinator, a node agent and a requester on one
//! Linux host; the requester leases the fixture stack's lane on the node,
//! syncs it, starts it there and waits until its API is healthy. Released,
//! the lane leaves nothing behind.
//!
//! Opt-in: it needs Docker and a static Linux build of devstack for the lane
//! container (the release's kind), named by `DEVSTACK_LANE_BIN`:
//!   DEVSTACK_INTEGRATION_NODE=1 DEVSTACK_LANE_BIN=<musl build>

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
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let ps = Command::new("docker")
            .args(["ps", "-aq", "--filter", "label=devstack.lease"])
            .output()
            .unwrap();
        if ps.stdout.is_empty() {
            break;
        }
        assert!(Instant::now() < until, "the lane container stays");
        std::thread::sleep(Duration::from_secs(1));
    }
}
