//! The hub watches itself (#398): a thread outside the async runtime asks
//! the hub's own port for `/hub/ping` every [`EVERY`]. A hub whose runtime
//! is stuck still has the port, and the operating system takes connections
//! for it until its queue is full, so a refused or unanswered request is
//! the only sign. After [`STUCK_AFTER`] without an answer it writes a dump
//! of itself (Windows), says so in its log, starts the next hub and exits:
//! the dashboard comes back by itself, and the dump says where it hung.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use devstack_daemon::diag;

/// How often the hub asks itself.
const EVERY: Duration = Duration::from_secs(30);
/// How long it waits for its own answer.
const WITHIN: Duration = Duration::from_secs(10);
/// How long without an answer before it counts as stuck.
const STUCK_AFTER: Duration = Duration::from_secs(120);
/// Dumps kept in the hub's directory, the newest.
const KEEP: usize = 3;

pub fn start(port: u16, dir: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("hub-watchdog".into())
        .spawn(move || watch(port, &dir));
}

fn watch(port: u16, dir: &Path) {
    let mut failed = 0u32;
    loop {
        std::thread::sleep(EVERY);
        if answers(port) {
            failed = 0;
            continue;
        }
        failed += 1;
        if !stuck(failed) {
            continue;
        }
        let dump = dir.join(format!("hub-stuck-{}.dmp", crate::now_ms()));
        let written = devstack_proc::write_dump(&dump);
        diag::warn(
            "hub_stuck",
            serde_json::json!({
                "port": port,
                "silent_s": u64::from(failed) * EVERY.as_secs(),
                "dump": written.as_ref().ok().map(|()| dump.display().to_string()),
                "dump_error": written.as_ref().err().map(ToString::to_string),
            }),
        );
        prune(dir);
        successor();
        let _ = std::fs::remove_file(dir.join("hub.json"));
        std::process::exit(1);
    }
}

/// Whether `failed` checks in a row without an answer make the hub stuck.
fn stuck(failed: u32) -> bool {
    u64::from(failed) * EVERY.as_secs() >= STUCK_AFTER.as_secs()
}

/// Whether the hub answers an HTTP request on `port` at all (any status):
/// a connection the operating system took for it is not enough.
fn answers(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), WITHIN)
    else {
        return false;
    };
    let _ = s.set_read_timeout(Some(WITHIN));
    let _ = s.set_write_timeout(Some(WITHIN));
    if s.write_all(b"GET /hub/ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut head = [0u8; 12];
    matches!(s.read(&mut head), Ok(n) if is_http_answer(&head[..n]))
}

fn is_http_answer(head: &[u8]) -> bool {
    head.starts_with(b"HTTP/1.")
}

/// Start the next hub before this one exits: it waits for the lock (up to
/// 15 s), which is free once this process is gone.
fn successor() {
    let Ok(exe) = crate::devstack_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("hub")
        .current_dir(crate::state_dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = devstack_proc::spawn_detached(&mut cmd);
}

/// Keep the newest [`KEEP`] dumps.
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dumps: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_dump(p))
        .collect();
    // Named by the time they were written: the oldest sort first.
    dumps.sort();
    let extra = dumps.len().saturating_sub(KEEP);
    for p in dumps.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

fn is_dump(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("hub-stuck-") && n.ends_with(".dmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuck_after_two_minutes_of_silence() {
        assert!(!stuck(1));
        assert!(!stuck(3));
        assert!(stuck(4));
    }

    #[test]
    fn only_an_http_status_line_is_an_answer() {
        assert!(is_http_answer(b"HTTP/1.1 200"));
        assert!(!is_http_answer(b""));
        assert!(!is_http_answer(b"SSH-2.0-Op"));
    }

    #[test]
    fn a_silent_port_does_not_answer() {
        // Takes the connection (the operating system's queue) and says
        // nothing: what a hub with a stuck runtime looks like from outside.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (_s, _) = l.accept().unwrap();
            std::thread::sleep(WITHIN + Duration::from_secs(1));
        });
        assert!(!answers(port));
        t.join().unwrap();
    }

    #[test]
    fn the_newest_dumps_are_kept() {
        let d = tempfile::tempdir().unwrap();
        for t in [1, 2, 3, 4, 5] {
            std::fs::write(d.path().join(format!("hub-stuck-{t}.dmp")), b"").unwrap();
        }
        std::fs::write(d.path().join("hub.log"), b"").unwrap();
        prune(d.path());
        let mut left: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "hub-stuck-3.dmp",
                "hub-stuck-4.dmp",
                "hub-stuck-5.dmp",
                "hub.log"
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_dump_of_this_process_is_written() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("hub-stuck-1.dmp");
        devstack_proc::write_dump(&p).unwrap();
        let mut sig = [0u8; 4];
        std::fs::File::open(&p)
            .unwrap()
            .read_exact(&mut sig)
            .unwrap();
        assert_eq!(&sig, b"MDMP");
    }
}
