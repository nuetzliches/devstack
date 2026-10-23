//! Process ownership, checked from the outside (the spike (a) scenarios as tests).

use devstack_proc::{Options, Spec, ensure_console, spawn};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FIXTURE: &str = env!("CARGO_BIN_EXE_devstack-proc-fixture");

fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls; handle checked and closed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat.rfind(')').and_then(|i| stat[i + 2..].chars().next()) != Some('Z'),
            Err(_) => false,
        }
    }
    // Asked of `ps`, not of libproc: the check stays apart from the code it checks.
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|o| {
                let stat = String::from_utf8_lossy(&o.stdout);
                let stat = stat.trim();
                !stat.is_empty() && !stat.starts_with('Z')
            })
    }
}

fn wait_for(cond: impl Fn() -> bool, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

fn cgroup_root(test: &str) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let root = PathBuf::from("/sys/fs/cgroup")
        .join(format!("devstack-test-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&root).ok().map(|_| root)
}

fn opts(test: &str) -> Options {
    Options {
        cgroup_root: cgroup_root(test),
        shim: vec![FIXTURE.into(), "shim".into()],
    }
}

fn spec(dir: &Path, name: &str, extra: &str) -> Spec {
    Spec {
        name: name.into(),
        command: format!(
            "\"{FIXTURE}\" serve --markers \"{}\" --name {name} {extra}",
            dir.display()
        ),
        cwd: dir.to_path_buf(),
        env: Default::default(),
        log: std::fs::File::create(dir.join(format!("{name}.log"))).unwrap(),
    }
}

fn marker(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}

#[test]
fn graceful_stop_reaches_the_whole_tree() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut tree, _) = spawn(spec(dir.path(), "svc", "--child"), &opts("graceful")).unwrap();
    assert!(
        wait_for(
            || marker(dir.path(), "child.started"),
            Duration::from_secs(10)
        ),
        "child did not start"
    );
    assert!(tree.pids().len() >= 2, "tree: {:?}", tree.pids());
    let out = tree.stop(Duration::from_secs(10));
    assert!(out.graceful, "{out:?}");
    assert!(marker(dir.path(), "svc.stopped") && marker(dir.path(), "child.stopped"));
    assert!(!tree.is_alive());
}

#[test]
fn stop_reaches_only_its_own_tree() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let o = opts("independent");
    let (mut a, _) = spawn(spec(dir.path(), "a", ""), &o).unwrap();
    let (mut b, _) = spawn(spec(dir.path(), "b", ""), &o).unwrap();
    assert!(wait_for(
        || marker(dir.path(), "a.started") && marker(dir.path(), "b.started"),
        Duration::from_secs(10)
    ));
    assert!(a.stop(Duration::from_secs(10)).graceful);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        b.is_alive() && !marker(dir.path(), "b.stopped"),
        "b was hit by a's signal"
    );
    assert!(b.stop(Duration::from_secs(10)).graceful);
}

#[test]
fn a_service_ignoring_the_signal_is_killed_after_the_timeout() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut tree, _) = spawn(
        spec(dir.path(), "svc", "--stubborn --child"),
        &opts("stubborn"),
    )
    .unwrap();
    assert!(wait_for(
        || marker(dir.path(), "child.started"),
        Duration::from_secs(10)
    ));
    let pids = tree.pids();
    let out = tree.stop(Duration::from_millis(800));
    assert!(
        !out.graceful && out.elapsed >= Duration::from_millis(800),
        "{out:?}"
    );
    assert!(
        wait_for(|| pids.iter().all(|&p| !alive(p)), Duration::from_secs(5)),
        "survivors of {pids:?}"
    );
}

#[test]
fn dropping_the_handle_ends_the_tree() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut tree, _) = spawn(spec(dir.path(), "svc", "--child"), &opts("drop")).unwrap();
    assert!(wait_for(
        || marker(dir.path(), "child.started"),
        Duration::from_secs(10)
    ));
    let pids = tree.pids();
    drop(tree);
    assert!(
        wait_for(|| pids.iter().all(|&p| !alive(p)), Duration::from_secs(5)),
        "survivors of {pids:?}"
    );
}

#[test]
fn the_root_exit_code_is_reported() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec(dir.path(), "x", "");
    s.command = "exit 7".into(); // cmd and sh both understand it
    let (mut tree, _) = spawn(s, &opts("exit")).unwrap();
    let t0 = Instant::now();
    while tree.try_exit_code().is_none() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(tree.try_exit_code(), Some(7));
}

/// The ADR-0009 guarantee: when the supervisor dies hard, nothing of its trees
/// survives. On Linux without a writable cgroup a child that left the process
/// group with setsid escapes; that is the documented degradation.
#[test]
fn supervisor_death_ends_every_tree() {
    let dir = tempfile::tempdir().unwrap();
    let (state, markers) = (dir.path().join("state"), dir.path().join("markers"));
    std::fs::create_dir_all(&markers).unwrap();
    let cg = cgroup_root("death");
    let mut cmd = std::process::Command::new(FIXTURE);
    cmd.args(["supervise", "--state"])
        .arg(&state)
        .arg("--markers")
        .arg(&markers);
    if let Some(cg) = &cg {
        cmd.arg("--cgroup-root").arg(cg);
    }
    let mut sup = cmd.spawn().unwrap();
    assert!(
        wait_for(|| state.join("pids.json").exists(), Duration::from_secs(20)),
        "supervisor did not start its tree"
    );
    let pids: Vec<u32> =
        serde_json::from_slice(&std::fs::read(state.join("pids.json")).unwrap()).unwrap();
    let orphan: u32 = std::fs::read_to_string(markers.join("orphan.started"))
        .unwrap()
        .parse()
        .unwrap();

    // A second supervisor on the same state directory is refused (ADR-0016).
    let second = std::process::Command::new(FIXTURE)
        .args(["supervise", "--state"])
        .arg(&state)
        .arg("--markers")
        .arg(&markers)
        .status()
        .unwrap();
    assert_eq!(second.code(), Some(3));

    sup.kill().unwrap(); // TerminateProcess / SIGKILL: no cleanup code runs
    sup.wait().unwrap();

    let orphan_escapes = cfg!(unix) && cg.is_none();
    let must_die: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|&p| !(orphan_escapes && p == orphan))
        .chain((!orphan_escapes).then_some(orphan))
        .collect();
    let ok = wait_for(
        || must_die.iter().all(|&p| !alive(p)),
        Duration::from_secs(5),
    );
    if orphan_escapes && alive(orphan) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &orphan.to_string()])
            .status();
    }
    assert!(
        ok,
        "survivors: {:?}",
        must_die.iter().filter(|&&p| alive(p)).collect::<Vec<_>>()
    );

    // The lock is free again right away.
    devstack_proc::StateLock::acquire(&state).unwrap();
}

#[cfg(unix)]
#[test]
fn cleanup_spares_a_process_that_only_shares_a_number() {
    use std::os::unix::process::CommandExt;
    let mut decoy = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let rec = devstack_proc::Record {
        name: "old".into(),
        pgid: decoy.id() as i64,
        leader_start: u64::MAX,
        cgroup: None,
    };
    devstack_proc::cleanup(&[rec]);
    std::thread::sleep(Duration::from_millis(200));
    assert!(alive(decoy.id()), "cleanup killed an unrelated process");
    decoy.kill().unwrap();
    decoy.wait().unwrap();
}

/// Linux `PR_SET_PDEATHSIG` fires when the *thread* that started the child
/// exits, not only the process. A tree started from a short-lived thread (a
/// command run, a blocking task of the async runtime) must still live on.
#[test]
fn a_tree_outlives_the_thread_that_started_it() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_path_buf();
    let o = opts("thread");
    // The thread lives until the service runs (as a command run waiting for
    // health does), so the shim's death signal is armed before the thread ends.
    let mut tree = std::thread::spawn(move || {
        let (tree, _) = spawn(spec(&d, "svc", ""), &o).unwrap();
        assert!(wait_for(
            || marker(&d, "svc.started"),
            Duration::from_secs(10)
        ));
        std::thread::sleep(Duration::from_millis(300));
        tree
    })
    .join()
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        tree.is_alive(),
        "the tree died with the thread that started it"
    );
    assert!(tree.stop(Duration::from_secs(10)).graceful);
}

/// Plan §12: usage covers the whole tree, and CPU time only grows.
#[test]
fn usage_covers_the_whole_tree() {
    ensure_console().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut tree, _) = spawn(
        spec(dir.path(), "svc", "--child --alloc-mb 64 --burn-ms 300"),
        &opts("usage"),
    )
    .unwrap();
    assert!(wait_for(
        || marker(dir.path(), "svc.started") && marker(dir.path(), "child.started"),
        Duration::from_secs(10)
    ));
    let u1 = tree.usage();
    assert_eq!(u1.processes as usize, tree.pids().len(), "{u1:?}");
    assert!(u1.processes >= 2, "{u1:?}");
    assert!(
        u1.memory >= 64 * 1024 * 1024,
        "the 64 MiB are missing: {u1:?}"
    );
    assert!(u1.cpu_us >= 200_000, "the 300 ms burn is missing: {u1:?}");
    std::thread::sleep(Duration::from_millis(200));
    let u2 = tree.usage();
    assert!(u2.cpu_us >= u1.cpu_us, "{u1:?} -> {u2:?}");
    assert!(tree.stop(Duration::from_secs(10)).graceful);
}
