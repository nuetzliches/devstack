//! A plain TCP forward from a fixed port to another one on this machine: what
//! `promote --lane` puts on the main ports of services without devstack's HTTP
//! proxy (dev servers, `proxy = false`). Bytes only, so TLS and WebSockets of a
//! dev server pass unchanged (ADR-0024).

use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};

/// Forwarding while it lives; dropping it stops the listeners a moment later,
/// `close` before it returns (open connections end on their own).
pub struct Forward {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Forward {
    /// Stop listening and wait until the listeners are gone, so the port can
    /// be bound again right away (an aborted task drops its listener only
    /// when the runtime gets to it).
    pub fn close(mut self) {
        let tasks = std::mem::take(&mut self.tasks);
        for t in &tasks {
            t.abort();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while !tasks.iter().all(|t| t.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Listen on `port` (127.0.0.1, and ::1 where a dev server may be reached as
/// `localhost`) and forward every connection to the lane's `target` port, on
/// IPv4 loopback or, where the dev server listens only there, IPv6 loopback
/// (Node resolves `localhost` to `::1` first).
pub fn start(port: u16, target: u16, rt: &tokio::runtime::Handle) -> std::io::Result<Forward> {
    // The port was just released by a stopped dev server or a closed forward;
    // the OS may need a moment.
    let deadline = Instant::now() + Duration::from_secs(2);
    let v4 = loop {
        match rt.block_on(TcpListener::bind(("127.0.0.1", port))) {
            Ok(l) => break l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    };
    let v6 = rt.block_on(TcpListener::bind(("::1", port))).ok();
    let mut tasks = vec![rt.spawn(accept(v4, port, target))];
    if let Some(l) = v6 {
        tasks.push(rt.spawn(accept(l, port, target)));
    }
    Ok(Forward { tasks })
}

async fn accept(listener: TcpListener, port: u16, target: u16) {
    loop {
        let (mut inbound, _) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                // A connection reset before it was accepted, or no file
                // handles left: say so, and keep listening.
                crate::diag::warn(
                    "forward_accept_failed",
                    serde_json::json!({ "port": port, "error": e.to_string() }),
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        tokio::spawn(async move {
            if let Some(mut outbound) = connect(target).await {
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            }
        });
    }
}

async fn connect(port: u16) -> Option<TcpStream> {
    let v4: SocketAddr = ([127, 0, 0, 1], port).into();
    let v6: SocketAddr = (std::net::Ipv6Addr::LOCALHOST, port).into();
    match TcpStream::connect(v4).await {
        Ok(s) => Some(s),
        Err(_) => TcpStream::connect(v6).await.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn bytes_pass_both_ways_and_the_port_is_free_after_drop() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // An echo server as the target.
        let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in echo.incoming().flatten() {
                let mut s = s;
                let mut b = [0u8; 5];
                if s.read_exact(&mut b).is_ok() {
                    let _ = s.write_all(&b);
                }
            }
        });
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let fwd = start(port, target, rt.handle()).unwrap();
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"hello").unwrap();
        let mut back = [0u8; 5];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"hello");
        fwd.close();
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "port released"
        );
    }

    #[test]
    fn a_target_on_ipv6_loopback_only_is_reached() {
        let Ok(echo) = std::net::TcpListener::bind("[::1]:0") else {
            return; // no IPv6 on this machine
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in echo.incoming().flatten() {
                let mut s = s;
                let _ = s.write_all(b"v6");
            }
        });
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let fwd = start(port, target, rt.handle()).unwrap();
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut back = [0u8; 2];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"v6");
        fwd.close();
    }
}
