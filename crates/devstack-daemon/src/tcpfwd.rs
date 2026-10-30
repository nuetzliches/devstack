//! A plain TCP forward from a fixed port to another one on this machine: what
//! `promote --lane` puts on the main ports of services without devstack's HTTP
//! proxy (dev servers, `proxy = false`). Bytes only, so TLS and WebSockets of a
//! dev server pass unchanged (ADR-0024).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// Forwarding while it lives; dropping it stops the listeners a moment later,
/// `close` before it returns (open connections end on their own).
pub struct Forward {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// The port on this machine it forwards to (`None`: an address
    /// elsewhere, [`start_to`]).
    target: Option<u16>,
}

impl Forward {
    /// The port on this machine it forwards to, if it forwards to one.
    pub fn target(&self) -> Option<u16> {
        self.target
    }

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
    Ok(Forward {
        tasks,
        target: Some(target),
    })
}

/// Listen on `port` (127.0.0.1) and forward every connection to `addr`
/// (`scheduler:4000`): in a lane on a node, its containers are on the lane's
/// network, and this puts each on the loopback port it has on the
/// developer's machine, so the services see the same addresses there as
/// here (ADR-0029 point 4).
pub fn start_to(port: u16, addr: String, rt: &tokio::runtime::Handle) -> std::io::Result<Forward> {
    let v4 = rt.block_on(TcpListener::bind(("127.0.0.1", port)))?;
    let task = rt.spawn(async move {
        loop {
            let Ok((mut inbound, _)) = v4.accept().await else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            let _ = inbound.set_nodelay(true);
            let addr = addr.clone();
            tokio::spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(addr.as_str()).await {
                    let _ = outbound.set_nodelay(true);
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    Ok(Forward {
        tasks: vec![task],
        target: None,
    })
}

/// What a [`Front`] does with a new connection (ADR-0043).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontState {
    /// Wait up to this long until there is a target (the dev server starts).
    Hold(Duration),
    /// Forward to this port.
    To(u16),
    /// Close it at once (the dev server is stopped).
    Refuse,
}

/// A dev server's fixed port, held by devstack and forwarded byte for byte
/// to its instance's port (ADR-0043): it holds new connections while the
/// dev server starts, forwards them while it runs and refuses them while it
/// is stopped. Switched without closing its listener.
pub struct Front {
    port: u16,
    state: Arc<watch::Sender<FrontState>>,
    active: Arc<AtomicUsize>,
    served: Arc<AtomicU64>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Front {
    /// Listen on `port` (127.0.0.1, and ::1 where it can), refusing until
    /// told otherwise.
    pub fn bind(port: u16, rt: &tokio::runtime::Handle) -> std::io::Result<Front> {
        let v4 = rt.block_on(TcpListener::bind(("127.0.0.1", port)))?;
        let v6 = rt.block_on(TcpListener::bind(("::1", port))).ok();
        Ok(Self::serve(port, std::iter::once(v4).chain(v6), rt))
    }

    /// Listen on `port` where Docker would publish it (ADR-0048): every
    /// interface without a `host` (IPv4, and IPv6 where it can), loopback
    /// for `127.0.0.1` (and `::1`), else that address. A container just let
    /// the port go: the OS may need a moment.
    pub fn bind_at(
        host: Option<&str>,
        port: u16,
        rt: &tokio::runtime::Handle,
    ) -> std::io::Result<Front> {
        let (first, second) = match host {
            None | Some("0.0.0.0") => ("0.0.0.0", Some("::")),
            Some("127.0.0.1") | Some("localhost") => ("127.0.0.1", Some("::1")),
            Some(h) => (h, None),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let main = loop {
            match rt.block_on(TcpListener::bind((first, port))) {
                Ok(l) => break l,
                Err(e)
                    if e.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        };
        // IPv6 beside IPv4: where the OS binds both on one socket, this one
        // is taken already, and the first serves both.
        let other = second.and_then(|h| rt.block_on(TcpListener::bind((h, port))).ok());
        Ok(Self::serve(port, std::iter::once(main).chain(other), rt))
    }

    fn serve(
        port: u16,
        listeners: impl Iterator<Item = TcpListener>,
        rt: &tokio::runtime::Handle,
    ) -> Front {
        let (tx, _) = watch::channel(FrontState::Refuse);
        let state = Arc::new(tx);
        let active = Arc::new(AtomicUsize::new(0));
        let served = Arc::new(AtomicU64::new(0));
        let mut tasks = Vec::new();
        for l in listeners {
            tasks.push(rt.spawn(front_accept(
                l,
                port,
                state.clone(),
                active.clone(),
                served.clone(),
            )));
        }
        Front {
            port,
            state,
            active,
            served,
            tasks,
        }
    }

    pub fn set(&self, state: FrontState) {
        self.state.send_replace(state);
    }

    pub fn state(&self) -> FrontState {
        self.state.borrow().clone()
    }

    /// As a proxy's view shows it: fixed port, instance port, connections.
    pub fn view(&self) -> crate::proxy::ProxyView {
        crate::proxy::ProxyView {
            port: self.port,
            upstream: match self.state() {
                FrontState::To(p) => Some(p),
                _ => None,
            },
            build: None,
            pending: None,
            active: self.active.load(Ordering::SeqCst),
            served: self.served.load(Ordering::SeqCst),
            last_error: None,
        }
    }

    /// Stop listening, and wait until the port is free again.
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

impl Drop for Front {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

async fn front_accept(
    listener: TcpListener,
    port: u16,
    state: Arc<watch::Sender<FrontState>>,
    active: Arc<AtomicUsize>,
    served: Arc<AtomicU64>,
) {
    loop {
        let mut inbound = match listener.accept().await {
            Ok((c, _)) => {
                let _ = c.set_nodelay(true);
                c
            }
            Err(e) => {
                crate::diag::warn_limited(
                    &format!("forward_accept_failed:{port}"),
                    "forward_accept_failed",
                    serde_json::json!({ "port": port, "error": e.to_string() }),
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let mut rx = state.subscribe();
        let (active, served) = (active.clone(), served.clone());
        tokio::spawn(async move {
            let Some(target) = target_of(&mut rx).await else {
                return; // refused: dropped, the client sees it closed
            };
            // The instance may still be binding its port: a few tries.
            let mut outbound = None;
            for _ in 0..20 {
                if let Some(s) = connect(target).await {
                    outbound = Some(s);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let Some(mut outbound) = outbound else {
                return;
            };
            active.fetch_add(1, Ordering::SeqCst);
            served.fetch_add(1, Ordering::SeqCst);
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Where a new connection goes: the target now, or once there is one while
/// held; `None` when refused or held past its deadline.
async fn target_of(rx: &mut watch::Receiver<FrontState>) -> Option<u16> {
    let mut until: Option<tokio::time::Instant> = None;
    loop {
        let now = rx.borrow_and_update().clone();
        match now {
            FrontState::To(p) => return Some(p),
            FrontState::Refuse => return None,
            FrontState::Hold(d) => {
                let deadline = *until.get_or_insert_with(|| tokio::time::Instant::now() + d);
                match tokio::time::timeout_at(deadline, rx.changed()).await {
                    Ok(Ok(())) => continue,
                    _ => return None,
                }
            }
        }
    }
}

async fn accept(listener: TcpListener, port: u16, target: u16) {
    loop {
        let (mut inbound, _) = match listener.accept().await {
            Ok(c) => {
                let _ = c.0.set_nodelay(true);
                c
            }
            Err(e) => {
                // A connection reset before it was accepted, or no file
                // handles left: say so, and keep listening.
                crate::diag::warn_limited(
                    &format!("forward_accept_failed:{port}"),
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
    let s = match TcpStream::connect(v4).await {
        Ok(s) => s,
        Err(_) => TcpStream::connect(v6).await.ok()?,
    };
    let _ = s.set_nodelay(true);
    Some(s)
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

    fn echo() -> u16 {
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
        target
    }

    fn free() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn a_front_holds_forwards_switches_and_refuses() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let port = free();
        let front = Front::bind(port, rt.handle()).unwrap();
        let ask = move || {
            let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let _ = c.write_all(b"hello");
            let mut back = [0u8; 5];
            c.read_exact(&mut back).map(|_| back)
        };
        // Refused: closed at once.
        assert!(ask().is_err());
        // Held: the connection waits for its target, then goes through.
        front.set(FrontState::Hold(Duration::from_secs(10)));
        let a = echo();
        let waiting = std::thread::spawn(ask);
        std::thread::sleep(Duration::from_millis(300));
        front.set(FrontState::To(a));
        assert_eq!(&waiting.join().unwrap().unwrap(), b"hello");
        // Switched to another instance, the listener stays.
        let b = echo();
        front.set(FrontState::To(b));
        assert_eq!(&ask().unwrap(), b"hello");
        assert_eq!(front.view().upstream, Some(b));
        assert_eq!(front.view().served, 2);
        // Held past its deadline: closed.
        front.set(FrontState::Hold(Duration::from_millis(200)));
        assert!(ask().is_err());
        front.close();
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
