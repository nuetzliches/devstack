//! UDP ports of a lane on a node, relayed through its tunnel (ADR-0056).
//!
//! A UDP session is a tunnel of its own: an HTTP upgrade
//! ([`PROTOCOL`]) on the lane daemon's `/v1/tunnel/{port}`, through the node
//! agent as a TCP tunnel goes. On it each datagram is a frame, its length as
//! two bytes, big-endian, then the datagram.
//!
//! - [`relay`], on the requester: a socket on the lane's port, a session per
//!   client address, each opening its tunnel with the first datagram and
//!   ending after [`Limits::idle`] without traffic.
//! - [`serve`], in the lane daemon: a session's socket towards the
//!   container's port.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

/// The `Upgrade` protocol of a UDP session's tunnel.
pub const PROTOCOL: &str = "devstack-udp";

/// The largest datagram a frame carries: its length is two bytes. Every
/// UDP datagram fits (65,507 bytes over IPv4, 65,527 over IPv6).
pub const MAX_DATAGRAM: usize = u16::MAX as usize;

/// What a socket is read with: any datagram the system delivers, whole.
const BUFFER: usize = 1 << 16;

/// Make `socket` able to send and receive any datagram a frame carries.
/// macOS refuses to send one larger than the socket's send buffer, 9 KiB
/// by default (the sysctl `net.inet.udp.maxdgram`), and drops one that does
/// not fit its receive buffer. A buffer is never made smaller.
pub fn room_for_any_datagram(socket: &UdpSocket) {
    let s = socket2::SockRef::from(socket);
    if s.send_buffer_size().is_ok_and(|n| n < BUFFER) {
        let _ = s.set_send_buffer_size(BUFFER);
    }
    // Room for a few, with what the system counts beside their bytes.
    if s.recv_buffer_size().is_ok_and(|n| n < 4 * BUFFER) {
        let _ = s.set_recv_buffer_size(4 * BUFFER);
    }
}

/// A datagram longer than a frame carries.
#[derive(Debug, PartialEq, Eq)]
pub struct TooLarge(pub usize);

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a datagram of {} bytes is larger than {MAX_DATAGRAM}",
            self.0
        )
    }
}

/// `datagram` as a frame, appended to `out`.
pub fn encode(datagram: &[u8], out: &mut Vec<u8>) -> Result<(), TooLarge> {
    let len = u16::try_from(datagram.len()).map_err(|_| TooLarge(datagram.len()))?;
    out.reserve(2 + datagram.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(datagram);
    Ok(())
}

/// Write `datagram` as one frame, in one write, and flush it: a datagram
/// waits for nothing after it.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, datagram: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::new();
    encode(datagram, &mut frame).map_err(|e| std::io::Error::other(e.to_string()))?;
    w.write_all(&frame).await?;
    w.flush().await
}

/// Read the next frame's datagram into `buf`. `false`: the stream ended
/// between frames; a stream that ends inside one is an error.
pub async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<bool> {
    let mut len = [0u8; 2];
    match r.read(&mut len[..1]).await? {
        0 => return Ok(false),
        _ => r.read_exact(&mut len[1..]).await?,
    };
    buf.resize(u16::from_be_bytes(len) as usize, 0);
    r.read_exact(buf).await?;
    Ok(true)
}

/// How a relay on the requester holds its sessions (ADR-0056 points 2, 3).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// A session ends after this long without a datagram either way.
    pub idle: Duration,
    /// Datagrams of a session that wait while its tunnel opens, or the
    /// node is slower than its client; more are dropped.
    pub queued: usize,
    /// Sessions per socket; a further client's datagrams are dropped.
    pub sessions: usize,
    /// A client whose tunnel could not be opened is tried again after this.
    pub retry: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(60),
            queued: 64,
            sessions: 256,
            retry: Duration::from_secs(1),
        }
    }
}

/// What becomes of a datagram from a client, by [`Table::route`].
#[derive(Debug)]
enum Route {
    /// Its session runs: into its queue.
    Session(mpsc::Sender<Vec<u8>>),
    /// A new session, for the caller to open.
    Open,
    /// Dropped: the client's tunnel failed a moment ago.
    Backoff,
    /// Dropped: the socket has as many sessions as it takes.
    Full,
}

/// A relay's sessions, by the client's address.
#[derive(Debug)]
struct Table {
    sessions: HashMap<SocketAddr, (u64, mpsc::Sender<Vec<u8>>)>,
    /// Clients whose tunnel could not be opened, and when.
    refused: HashMap<SocketAddr, Instant>,
    next: u64,
    limits: Limits,
}

impl Table {
    fn new(limits: Limits) -> Self {
        Self {
            sessions: HashMap::new(),
            refused: HashMap::new(),
            next: 0,
            limits,
        }
    }

    fn route(&mut self, from: SocketAddr, now: Instant) -> Route {
        if let Some((_, tx)) = self.sessions.get(&from) {
            if !tx.is_closed() {
                return Route::Session(tx.clone());
            }
            // Ended a moment ago, not removed yet: a new one.
            self.sessions.remove(&from);
        }
        if let Some(at) = self.refused.get(&from) {
            if now.duration_since(*at) < self.limits.retry {
                return Route::Backoff;
            }
            self.refused.remove(&from);
        }
        if self.sessions.len() >= self.limits.sessions {
            self.sessions.retain(|_, (_, tx)| !tx.is_closed());
            if self.sessions.len() >= self.limits.sessions {
                return Route::Full;
            }
        }
        Route::Open
    }

    /// A session for `from`: its id, and where its datagrams go.
    fn open(&mut self, from: SocketAddr) -> (u64, mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::channel(self.limits.queued.max(1));
        self.sessions.insert(from, (self.next, tx.clone()));
        (self.next, tx, rx)
    }

    /// Session `id` of `from` is over; `refused`: its tunnel did not open.
    fn ended(&mut self, from: SocketAddr, id: u64, refused: Option<Instant>) {
        if self.sessions.get(&from).is_some_and(|(i, _)| *i == id) {
            self.sessions.remove(&from);
        }
        if let Some(at) = refused {
            // Forget the old ones, so a scan does not keep them all.
            let retry = self.limits.retry;
            self.refused.retain(|_, t| at.duration_since(*t) < retry);
            self.refused.insert(from, at);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.sessions.len()
    }
}

/// A session's tunnel, as the requester opened it.
pub struct Opened<S> {
    pub stream: S,
    /// Told the bytes the session carried when it ends (telemetry).
    pub closed: Option<Box<dyn FnOnce(u64) + Send>>,
}

/// Relay `socket` (a lane's UDP port on the requester) until this future
/// is dropped: each client's datagrams go through a tunnel `open` opens
/// for it, what comes back goes to that client. Dropped, every session
/// ends and the socket is let go. `report` hears what goes wrong, a tunnel
/// that does not open.
pub async fn relay<O, F, S>(
    socket: UdpSocket,
    open: O,
    limits: Limits,
    report: Arc<dyn Fn(String) + Send + Sync>,
) where
    O: Fn() -> F + Send + Sync + 'static,
    F: std::future::Future<Output = Result<Opened<S>, String>> + Send + 'static,
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    room_for_any_datagram(&socket);
    let socket = Arc::new(socket);
    let open = Arc::new(open);
    let table = Arc::new(Mutex::new(Table::new(limits)));
    // Its sessions end with it.
    let (stop, _) = watch::channel(false);
    let _stop = StopOnDrop(stop.clone());
    let mut buf = vec![0u8; BUFFER];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(x) => x,
            // Windows reports a reply that found no client (ICMP port
            // unreachable) on the next read: not this socket's end.
            Err(e) if refused(&e) => continue,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let datagram = buf[..n].to_vec();
        let route = table.lock().unwrap().route(from, Instant::now());
        match route {
            Route::Session(tx) => {
                // Full: the tunnel lags; dropped, as a network would.
                let _ = tx.try_send(datagram);
            }
            Route::Backoff | Route::Full => {}
            Route::Open => {
                let (id, tx, rx) = table.lock().unwrap().open(from);
                let _ = tx.try_send(datagram);
                drop(tx);
                let (socket, open, table, report) =
                    (socket.clone(), open.clone(), table.clone(), report.clone());
                let mut stopped = stop.subscribe();
                tokio::spawn(async move {
                    let session = async {
                        let opened = match open().await {
                            Ok(o) => o,
                            Err(e) => {
                                report(e);
                                return Some(Instant::now());
                            }
                        };
                        let bytes = carry(opened.stream, rx, &socket, from, limits.idle).await;
                        if let Some(closed) = opened.closed {
                            closed(bytes);
                        }
                        None
                    };
                    let refused = tokio::select! {
                        r = session => r,
                        _ = stopped.wait_for(|s| *s) => None,
                    };
                    table.lock().unwrap().ended(from, id, refused);
                });
            }
        }
    }
}

struct StopOnDrop(watch::Sender<bool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// A port-unreachable error a UDP socket reports after a datagram it sent:
/// that datagram is lost, the socket goes on.
fn refused(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
    )
}

/// The time of the last datagram either way, shared by both directions.
struct Activity(Mutex<Instant>);

impl Activity {
    fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }

    fn touch(&self) {
        *self.0.lock().unwrap() = Instant::now();
    }

    /// Returns once nothing went either way for `idle`.
    async fn idle(&self, idle: Duration) {
        loop {
            let until = *self.0.lock().unwrap() + idle;
            if Instant::now() >= until {
                return;
            }
            tokio::time::sleep_until(until.into()).await;
        }
    }
}

/// One session on the requester: `rx`'s datagrams into `stream` as frames,
/// its frames to `client`, until the tunnel ends or nothing went either
/// way for `idle`. Returns the bytes carried.
async fn carry<S>(
    stream: S,
    mut rx: mpsc::Receiver<Vec<u8>>,
    socket: &UdpSocket,
    client: SocketAddr,
    idle: Duration,
) -> u64
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let activity = Activity::new();
    let bytes = std::sync::atomic::AtomicU64::new(0);
    let count = |n: usize| bytes.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    let up = async {
        while let Some(d) = rx.recv().await {
            if write_frame(&mut wr, &d).await.is_err() {
                return;
            }
            count(d.len());
            activity.touch();
        }
    };
    let down = async {
        let mut buf = Vec::new();
        while let Ok(true) = read_frame(&mut rd, &mut buf).await {
            activity.touch();
            count(buf.len());
            let _ = socket.send_to(&buf, client).await;
        }
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = activity.idle(idle) => {}
    }
    bytes.load(std::sync::atomic::Ordering::Relaxed)
}

/// One session in the lane daemon: `stream`'s frames to `target` (a socket
/// connected to the container's port), what comes back as frames, until
/// the tunnel ends or nothing went either way for `idle` (a backstop: the
/// requester ends a session first). A port-unreachable error (the
/// container restarts) loses that datagram, not the session.
pub async fn serve<S>(stream: S, target: UdpSocket, idle: Duration)
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let activity = Activity::new();
    let up = async {
        let mut buf = Vec::new();
        while let Ok(true) = read_frame(&mut rd, &mut buf).await {
            activity.touch();
            let _ = target.send(&buf).await;
        }
    };
    let down = async {
        let mut buf = vec![0u8; BUFFER];
        loop {
            match target.recv(&mut buf).await {
                Ok(n) => {
                    activity.touch();
                    if write_frame(&mut wr, &buf[..n]).await.is_err() {
                        return;
                    }
                }
                Err(e) if refused(&e) => continue,
                Err(_) => return,
            }
        }
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = activity.idle(idle) => {}
    }
}

/// A UDP socket for a session towards `addr` (`dns:53`, a container on the
/// lane's network): bound to any port of the address's family, connected,
/// so only `addr` is heard. Towards a loopback address it is bound there:
/// a socket on every address makes Windows' firewall ask (the tests relay to
/// a local echo).
pub async fn connect(addr: &str) -> std::io::Result<UdpSocket> {
    let to = tokio::net::lookup_host(addr)
        .await?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("{addr} has no address")))?;
    let any: SocketAddr = match (to.ip().is_loopback(), to.is_ipv4()) {
        (true, true) => (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        (true, false) => (std::net::Ipv6Addr::LOCALHOST, 0).into(),
        (false, true) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
        (false, false) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = UdpSocket::bind(any).await?;
    room_for_any_datagram(&socket);
    socket.connect(to).await?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_datagram_is_its_length_and_its_bytes() {
        let mut out = Vec::new();
        encode(b"dns?", &mut out).unwrap();
        encode(b"", &mut out).unwrap();
        assert_eq!(out, [0, 4, b'd', b'n', b's', b'?', 0, 0]);
        let big = vec![7u8; MAX_DATAGRAM];
        let mut out = Vec::new();
        encode(&big, &mut out).unwrap();
        assert_eq!(&out[..2], &[0xff, 0xff]);
        assert_eq!(out.len(), 2 + MAX_DATAGRAM);
        assert_eq!(
            encode(&vec![0u8; MAX_DATAGRAM + 1], &mut Vec::new()),
            Err(TooLarge(MAX_DATAGRAM + 1))
        );
    }

    #[tokio::test]
    async fn frames_read_back_as_the_datagrams_written() {
        let (mut a, mut b) = tokio::io::duplex(1 << 18);
        let sent: Vec<Vec<u8>> = vec![b"one".to_vec(), Vec::new(), vec![9u8; 65_507]];
        for d in &sent {
            write_frame(&mut a, d).await.unwrap();
        }
        drop(a);
        let mut buf = Vec::new();
        for d in &sent {
            assert!(read_frame(&mut b, &mut buf).await.unwrap());
            assert_eq!(&buf, d);
        }
        assert!(
            !read_frame(&mut b, &mut buf).await.unwrap(),
            "ends between frames"
        );
    }

    #[tokio::test]
    async fn a_stream_that_ends_inside_a_frame_is_an_error() {
        for cut in [&[0u8][..], &[0, 5, b'a', b'b'][..]] {
            let mut r = cut;
            let e = read_frame(&mut r, &mut Vec::new()).await.unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof, "{cut:?}");
        }
    }

    fn addr(port: u16) -> SocketAddr {
        ([127, 0, 0, 1], port).into()
    }

    #[test]
    fn sessions_are_keyed_by_the_clients_address() {
        let mut t = Table::new(Limits::default());
        let now = Instant::now();
        assert!(matches!(t.route(addr(1000), now), Route::Open));
        let (id, _tx, _rx) = t.open(addr(1000));
        // The same client: its session; another port is another client.
        assert!(matches!(t.route(addr(1000), now), Route::Session(_)));
        assert!(matches!(t.route(addr(1001), now), Route::Open));
        let (other, _tx2, _rx2) = t.open(addr(1001));
        assert_eq!(t.len(), 2);
        // An old session's end does not remove a newer one of the client.
        t.ended(addr(1000), id + 100, None);
        assert_eq!(t.len(), 2);
        t.ended(addr(1001), other, None);
        assert_eq!(t.len(), 1);
        assert!(matches!(t.route(addr(1001), now), Route::Open));
    }

    #[test]
    fn a_session_whose_queue_closed_is_opened_again() {
        let mut t = Table::new(Limits::default());
        let (_, tx, rx) = t.open(addr(1000));
        drop((tx, rx));
        assert!(matches!(t.route(addr(1000), Instant::now()), Route::Open));
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn a_refused_client_waits_and_a_full_table_drops() {
        let limits = Limits {
            sessions: 2,
            retry: Duration::from_secs(1),
            ..Limits::default()
        };
        let mut t = Table::new(limits);
        let now = Instant::now();
        let (id, _, _) = t.open(addr(1));
        t.ended(addr(1), id, Some(now));
        assert!(matches!(t.route(addr(1), now), Route::Backoff));
        assert!(matches!(
            t.route(addr(1), now + Duration::from_millis(999)),
            Route::Backoff
        ));
        assert!(matches!(
            t.route(addr(1), now + Duration::from_secs(1)),
            Route::Open
        ));
        let (_, _a, _ra) = t.open(addr(2));
        let (_, _b, _rb) = t.open(addr(3));
        assert!(matches!(t.route(addr(4), now), Route::Full));
        // One that ended frees its place.
        drop((_b, _rb));
        assert!(matches!(t.route(addr(4), now), Route::Open));
    }

    async fn echo() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        room_for_any_datagram(&s);
        let at = s.local_addr().unwrap();
        let h = tokio::spawn(async move {
            let mut buf = vec![0u8; BUFFER];
            while let Ok((n, from)) = s.recv_from(&mut buf).await {
                let mut reply = b"echo:".to_vec();
                reply.extend_from_slice(&buf[..n]);
                let _ = s.send_to(&reply, from).await;
            }
        });
        (at, h)
    }

    /// A relay whose tunnels are in-process pipes to [`serve`] towards
    /// `target`: both ends of a session without HTTP between.
    async fn relay_to(
        target: SocketAddr,
        limits: Limits,
        node_idle: Duration,
    ) -> (
        SocketAddr,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = socket.local_addr().unwrap();
        let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = opened.clone();
        let open = move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                let (a, b) = tokio::io::duplex(1 << 17);
                let to = connect(&target.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::spawn(serve(b, to, node_idle));
                Ok(Opened {
                    stream: a,
                    closed: None,
                })
            }
        };
        let h = tokio::spawn(relay(socket, open, limits, Arc::new(|_| {})));
        (at, opened, h)
    }

    async fn ask(client: &UdpSocket, to: SocketAddr, what: &[u8]) -> Vec<u8> {
        room_for_any_datagram(client);
        client.send_to(what, to).await.unwrap();
        let mut buf = vec![0u8; BUFFER];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
            .await
            .expect("an answer")
            .unwrap();
        buf.truncate(n);
        buf
    }

    #[tokio::test]
    async fn a_session_carries_datagrams_both_ways_and_ends_when_idle() {
        let (target, _echo) = echo().await;
        let limits = Limits {
            idle: Duration::from_millis(300),
            ..Limits::default()
        };
        let (relay_at, opened, relay) = relay_to(target, limits, Duration::from_secs(60)).await;
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(ask(&a, relay_at, b"one").await, b"echo:one");
        assert_eq!(ask(&a, relay_at, b"two").await, b"echo:two");
        let big = vec![b'x'; 60_000];
        assert_eq!(ask(&a, relay_at, &big).await.len(), 60_005, "whole");
        assert_eq!(
            opened.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one session"
        );
        assert_eq!(ask(&b, relay_at, b"three").await, b"echo:three");
        assert_eq!(
            opened.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a client each"
        );
        // Idle for longer than the timeout: the next datagram opens anew.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(ask(&a, relay_at, b"four").await, b"echo:four");
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 3);
        relay.abort();
    }

    #[tokio::test]
    async fn the_node_side_ends_an_idle_session_and_the_next_datagram_reopens() {
        let (target, _echo) = echo().await;
        let (relay_at, opened, relay) =
            relay_to(target, Limits::default(), Duration::from_millis(200)).await;
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(ask(&a, relay_at, b"one").await, b"echo:one");
        // The tunnel ends from the other side (a lost tunnel looks alike).
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(ask(&a, relay_at, b"two").await, b"echo:two");
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 2);
        relay.abort();
    }

    #[tokio::test]
    async fn a_tunnel_that_does_not_open_drops_and_is_tried_again_later() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = socket.local_addr().unwrap();
        let tries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = tries.clone();
        let open = move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err::<Opened<tokio::io::DuplexStream>, _>("the container does not run".into()) }
        };
        let said = Arc::new(Mutex::new(Vec::new()));
        let s2 = said.clone();
        let limits = Limits {
            retry: Duration::from_millis(400),
            ..Limits::default()
        };
        let relay = tokio::spawn(relay(
            socket,
            open,
            limits,
            Arc::new(move |e| s2.lock().unwrap().push(e)),
        ));
        let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        for _ in 0..5 {
            c.send_to(b"x", at).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            tries.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "no hold, no storm"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.send_to(b"x", at).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(said.lock().unwrap()[0], "the container does not run");
        relay.abort();
    }

    #[tokio::test]
    async fn a_relay_dropped_lets_its_port_go() {
        let (target, _echo) = echo().await;
        let (relay_at, _, relay) =
            relay_to(target, Limits::default(), Duration::from_secs(60)).await;
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(ask(&a, relay_at, b"one").await, b"echo:one");
        relay.abort();
        let until = Instant::now() + Duration::from_secs(5);
        while UdpSocket::bind(relay_at).await.is_err() {
            assert!(Instant::now() < until, "the port stays held by a session");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
