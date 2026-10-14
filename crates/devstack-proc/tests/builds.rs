//! The shared build job (ADR-0020), checked from the outside.

use devstack_proc::{BuildJob, Options, Spec};
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
    #[cfg(unix)]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat.rfind(')').and_then(|i| stat[i + 2..].chars().next()) != Some('Z'),
            Err(_) => false,
        }
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

/// One cgroup per test: tests run in parallel in one process, and a dropped
/// build job kills its whole cgroup.
fn opts(test: &str) -> Options {
    let cgroup_root = if cfg!(target_os = "linux") {
        let root = PathBuf::from("/sys/fs/cgroup").join(format!(
            "devstack-test-builds-{test}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).ok().map(|_| root)
    } else {
        None
    };
    Options {
        cgroup_root,
        shim: vec![FIXTURE.into(), "shim".into()],
    }
}

fn spec(dir: &Path, name: &str, command: String) -> Spec {
    Spec {
        name: name.into(),
        command,
        cwd: dir.into(),
        env: Default::default(),
        log: std::fs::File::create(dir.join(format!("{name}.log"))).unwrap(),
    }
}

fn pid_of(dir: &Path, marker: &str) -> u32 {
    std::fs::read_to_string(dir.join(marker))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn a_build_reports_its_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let job = BuildJob::new(&opts("exit")).unwrap();
    let mut b = job.run(spec(dir.path(), "b", "exit 3".into())).unwrap();
    assert_eq!(b.wait().unwrap(), 3);
}

#[test]
fn cancel_ends_the_build_tree_but_not_the_build_servers() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().display().to_string();
    let o = opts("cancel");
    let job = BuildJob::new(&o).unwrap();

    // A first build leaves a "build server" behind and exits.
    let mut first = job
        .run(spec(
            dir.path(),
            "first",
            format!("\"{FIXTURE}\" launch --markers \"{d}\""),
        ))
        .unwrap();
    assert_eq!(first.wait().unwrap(), 0);
    let server = pid_of(dir.path(), "server.started");
    assert!(alive(server));

    // A second build with a child is cancelled.
    let mut second = job
        .run(spec(
            dir.path(),
            "second",
            format!("\"{FIXTURE}\" serve --markers \"{d}\" --name svc --child"),
        ))
        .unwrap();
    assert!(wait_for(
        || dir.path().join("child.started").exists(),
        Duration::from_secs(10)
    ));
    let (svc, child) = (
        pid_of(dir.path(), "svc.started"),
        pid_of(dir.path(), "child.started"),
    );
    second.cancel();
    assert!(
        wait_for(|| !alive(svc) && !alive(child), Duration::from_secs(5)),
        "the cancelled build's tree survived"
    );
    assert!(
        alive(server),
        "cancelling one build must not end the build servers"
    );

    // The job ends the servers with it, where something holds them (ADR-0020).
    let held = cfg!(windows) || o.cgroup_root.is_some();
    drop(job);
    if held {
        assert!(
            wait_for(|| !alive(server), Duration::from_secs(5)),
            "the build job did not end its servers"
        );
    } else {
        #[cfg(unix)]
        // SAFETY: plain syscall on a process this test started.
        unsafe {
            libc::kill(server as i32, libc::SIGKILL);
        }
    }
}
