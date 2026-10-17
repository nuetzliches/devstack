import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
R='D:/Sources/devstack/.claude/worktrees/review-daemon/'
apply(R+'crates/devstack/tests/daemon_lifecycle.rs',[
("""    // up again: same artifact, no build (ADR-0016).
    let artifacts = s.artifacts();
    let up = s.cli(&["up"], None); // `broken` fails again
    assert!(
        s.api("/info").is_some(),
        "api not back after up:\\n{}{}\\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr),
        s.diag()
    );
    assert_eq!(s.info()["build"], second_build.as_str());
    assert_eq!(s.artifacts(), artifacts, "no new build after a restart");

    // `restart --rebuild` builds the same inputs again under `<id>-2` (#13).
    s.ok(&["restart", "api", "--rebuild"]);
    assert!(
        wait_for(
            || s.api("/info")
                .is_some_and(|i| i.contains(&format!("{second_build}-2"))),
            Duration::from_secs(120)
        ),
        "rebuilt as {second_build}-2\\n{}",
        s.diag()
    );
    assert!(
        s.daemon_log().contains("build_forced"),
        "the daemon log says why it built"
    );""",
"""    // An edit while no daemon runs. `up` again: the last promoted artifact,
    // not built (ADR-0016); the edit then arrives as a candidate.
    std::fs::write(&program, format!("{source}\\n// edited while down\\n")).unwrap();
    let up = s.cli(&["up"], None); // `broken` fails again
    assert!(
        s.api("/info").is_some(),
        "api not back after up:\\n{}{}\\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr),
        s.diag()
    );
    assert!(
        s.daemon_log().lines().any(|l| l.contains("\\"event\\":\\"restored\\"")
            && l.contains(&format!("\\"build_id\\":\\"{second_build}\\""))),
        "restored from {second_build}, not built\\n{}",
        s.diag()
    );
    assert!(
        wait_for(
            || s.api("/info").is_some_and(|i| !i.contains(&second_build))
                && s.service_view("api")["candidate"].is_null(),
            Duration::from_secs(120)
        ),
        "the edit made while down arrives as a candidate\\n{}",
        s.diag()
    );
    let third_build = s.info()["build"].as_str().unwrap().to_string();

    // `restart --rebuild` builds the same inputs again under `<id>-2` (#13).
    s.ok(&["restart", "api", "--rebuild"]);
    assert!(
        wait_for(
            || s.api("/info")
                .is_some_and(|i| i.contains(&format!("{third_build}-2"))),
            Duration::from_secs(120)
        ),
        "rebuilt as {third_build}-2\\n{}",
        s.diag()
    );
    assert!(
        s.daemon_log().contains("build_forced"),
        "the daemon log says why it built"
    );
    // Also of a stopped service: it starts from the build just made, not
    // from the one it replaces.
    s.ok(&["stop", "api"]);
    s.ok(&["restart", "api", "--rebuild"]);
    assert!(
        s.api("/info")
            .is_some_and(|i| i.contains(&format!("{third_build}-3"))),
        "a stopped service starts from its rebuild\\n{}",
        s.diag()
    );"""),
("""    wait("the schema change is held", &|| {
        candidate()["state"] == "held"
    });
    assert_eq!(build(), b3, "held: main unchanged");
    s.ok(&["promote", "api"]);
    assert_ne!(build(), b3, "promoted");
    assert!(candidate().is_null());""",
"""    wait("the schema change is held", &|| {
        candidate()["state"] == "held"
    });
    assert_eq!(build(), b3, "held: main unchanged");
    let held = candidate()["build_id"].as_str().unwrap().to_string();
    // A harmless edit afterwards carries the schema change too: it waits in
    // the held one's place, it is not promoted on its own (ADR-0013).
    std::fs::write(&program, format!("{source}\\n// edit 4\\n// edit 5\\n")).unwrap();
    wait("the newer build is held in its place", &|| {
        let c = candidate();
        c["state"] == "held" && c["build_id"].as_str().is_some_and(|b| b != held)
    });
    assert_eq!(build(), b3, "still held: main unchanged");
    let newest = candidate()["build_id"].as_str().unwrap().to_string();
    s.ok(&["promote", "api"]);
    assert_eq!(build(), newest, "promote takes the newest held build");
    assert!(candidate().is_null());"""),
("""/// GET on a port of this machine, the body.
fn get_on(port: u16, path: &str) -> Option<String> {""",
"""/// A job that hangs ends at its `timeout`; one without is ended by `stop`;
/// an `exec` step at its `timeout` (D-6). Needs no SDK.
#[test]
fn a_hanging_job_or_step_ends() {
    if !enabled("DEVSTACK_INTEGRATION") {
        eprintln!("skipped: set DEVSTACK_INTEGRATION=1");
        return;
    }
    let s = Stack::new(false);
    let hang = if cfg!(windows) {
        "ping -n 60 127.0.0.1 >NUL"
    } else {
        "sleep 60"
    };
    std::fs::write(
        s.file(),
        format!(
            r#"version = 1
[stack]
name = "hang"
[services.migrate]
kind = "job"
run = "{hang}"
timeout = "2s"
[services.seed]
kind = "job"
run = "{hang}"
autostart = false
[commands.slow]
description = "hangs"
steps = [{{ exec = "{hang}", timeout = "1s" }}]
"#
        ),
    )
    .unwrap();
    let t0 = Instant::now();
    let up = s.cli(&["up"], None);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(!up.status.success(), "{text}");
    assert!(t0.elapsed() < Duration::from_secs(40), "up returned");
    let status = s.ok(&["status"]);
    assert!(status.contains("timeout"), "{status}");

    // A start that waits for a job without a timeout: `stop` ends it.
    let file = s.file();
    let home = s.token_home.path().to_path_buf();
    let start = std::thread::spawn(move || {
        Command::new(BIN)
            .args(["start", "seed", "-f"])
            .arg(&file)
            .env("LOCALAPPDATA", &home)
            .env("XDG_STATE_HOME", &home)
            .env("DEVSTACK_NO_HUB", "1")
            .output()
            .unwrap()
    });
    assert!(
        wait_for(
            || s.service_view("seed")["status"]["state"] == "starting",
            Duration::from_secs(20)
        ),
        "{}",
        s.diag()
    );
    let t0 = Instant::now();
    s.ok(&["stop", "seed"]);
    let out = start.join().unwrap();
    assert!(!out.status.success(), "the start was ended");
    assert!(t0.elapsed() < Duration::from_secs(15), "at once, not after the job");
    assert_eq!(s.service_view("seed")["status"]["state"], "stopped");

    // An `exec` step ends at its timeout.
    let run = s.cli(&["run", "slow"], None);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!run.status.success() && text.contains("timeout"), "{text}");
    s.ok(&["down"]);
}

/// GET on a port of this machine, the body.
fn get_on(port: u16, path: &str) -> Option<String> {"""),
])
print("ok")
