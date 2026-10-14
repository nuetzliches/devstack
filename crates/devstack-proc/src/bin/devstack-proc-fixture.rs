//! Test fixture for devstack-proc (not shipped). Modes:
//!
//! - `serve --markers DIR --name N [--child] [--detached-child] [--stubborn]`:
//!   a service that writes `N.started` and, on the graceful stop signal,
//!   `N.stopped` (unless stubborn), optionally with a child and a detached child.
//! - `supervise --state DIR --markers DIR [--cgroup-root DIR]`: owns one tree
//!   like the daemon does and runs until killed (for the supervisor-death test).
//! - `launch --markers DIR`: starts a long-lived `server` and exits, like a
//!   build that leaves a build server behind.
//! - `shim …`: the Linux shim.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("supervise") => supervise(&args[1..]),
        // Like a build that starts a build server and exits.
        Some("launch") => {
            let m = arg(&args[1..], "--markers").expect("--markers");
            spawn_self(&["serve", "--markers", &m, "--name", "server"], false);
            let marker = PathBuf::from(&m).join("server.started");
            while !marker.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(unix)]
        Some("shim") => devstack_proc::shim_main(&args[1..]),
        _ => {
            eprintln!("usage: devstack-proc-fixture serve|supervise|shim …");
            std::process::exit(2)
        }
    }
}

fn install_stop_handler() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe extern "system" fn handler(_: u32) -> i32 {
            STOP.store(true, Ordering::SeqCst);
            1
        }
        // SAFETY: registering a handler with the documented signature.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn handler(_: libc::c_int) {
            STOP.store(true, Ordering::SeqCst);
        }
        let h: extern "C" fn(libc::c_int) = handler;
        // SAFETY: the handler only touches an atomic.
        unsafe {
            libc::signal(libc::SIGTERM, h as libc::sighandler_t);
            libc::signal(libc::SIGINT, h as libc::sighandler_t);
        }
    }
}

// The children are left running on purpose: owning and ending them is what the
// tests check, not this process.
#[allow(clippy::zombie_processes)]
fn spawn_self(args: &[&str], detached: bool) {
    let exe = std::env::current_exe().expect("current exe");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null());
    if detached {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
    }
    cmd.spawn().expect("spawn child");
}

fn serve(args: &[String]) {
    let markers = PathBuf::from(arg(args, "--markers").expect("--markers"));
    let name = arg(args, "--name").unwrap_or_else(|| "svc".into());
    let stubborn = args.iter().any(|a| a == "--stubborn");
    install_stop_handler();
    std::fs::write(
        markers.join(format!("{name}.started")),
        std::process::id().to_string(),
    )
    .unwrap();
    let m = markers.to_string_lossy().to_string();
    if args.iter().any(|a| a == "--child") {
        spawn_self(&["serve", "--markers", &m, "--name", "child"], false);
    }
    if args.iter().any(|a| a == "--detached-child") {
        spawn_self(&["serve", "--markers", &m, "--name", "orphan"], true);
    }
    loop {
        if STOP.load(Ordering::SeqCst) && !stubborn {
            std::fs::write(markers.join(format!("{name}.stopped")), "graceful").unwrap();
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn supervise(args: &[String]) {
    let state = PathBuf::from(arg(args, "--state").expect("--state"));
    let markers = arg(args, "--markers").expect("--markers");
    devstack_proc::ensure_console().expect("console");
    let _lock = match devstack_proc::StateLock::acquire(&state) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(3);
        }
    };
    let exe = std::env::current_exe().unwrap();
    let opts = devstack_proc::Options {
        cgroup_root: arg(args, "--cgroup-root").map(PathBuf::from),
        shim: vec![exe.to_string_lossy().into_owned(), "shim".into()],
    };
    let spec = devstack_proc::Spec {
        name: "svc".into(),
        command: format!(
            "\"{}\" serve --markers \"{markers}\" --name svc --child --detached-child",
            exe.display()
        ),
        cwd: std::env::current_dir().unwrap(),
        env: Default::default(),
        log: std::fs::File::create(state.join("svc.log")).unwrap(),
    };
    let (mut tree, record) = devstack_proc::spawn(spec, &opts).expect("spawn");
    std::fs::write(
        state.join("records.json"),
        serde_json::to_vec(&[record]).unwrap(),
    )
    .unwrap();
    // Wait until child and orphan exist, then publish the members.
    for _ in 0..200 {
        let m = PathBuf::from(&markers);
        if m.join("child.started").exists() && m.join("orphan.started").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    std::fs::write(
        state.join("pids.json"),
        serde_json::to_vec(&tree.pids()).unwrap(),
    )
    .unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
