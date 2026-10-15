//! A plain TCP forward from a fixed port to another one on this machine: what
//! `promote --lane` puts on the main ports of services without devstack's HTTP
//! proxy (dev servers, `proxy = false`). Bytes only, so TLS and WebSockets of a
//! dev server pass unchanged (ADR-0024).

use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};

/// Forwarding while it lives; dropping it closes the listeners (open
/// connections end on their own).
pub struct Forward {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Listen on `port` (127.0.0.1, and ::1 where a dev server may be reached as
/// `localhost`) and forward every connection to `127.0.0.1:target`.
pub fn start(port: u16, target: u16, rt: &tokio::runtime::Handle) -> std::io::Result<Forward> {
    let v4 = rt.block_on(TcpListener::bind(("127.0.0.1", port)))?;
    let v6 = rt.block_on(TcpListener::bind(("::1", port))).ok();
    let to: SocketAddr = ([127, 0, 0, 1], target).into();
    let mut tasks = vec![rt.spawn(accept(v4, to))];
    if let Some(l) = v6 {
        tasks.push(rt.spawn(accept(l, to)));
    }
    Ok(Forward { tasks })
}

async fn accept(listener: TcpListener, to: SocketAddr) {
    while let Ok((mut inbound, _)) = listener.accept().await {
        tokio::spawn(async move {
            if let Ok(mut outbound) = TcpStream::connect(to).await {
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            }
        });
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
        drop(fwd);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "port released"
        );
    }
}
