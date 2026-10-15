//! The reverse proxy on a service's fixed port (ADR-0010, option B).
//!
//! Instances run on internal ports; the proxy picks the upstream per request,
//! so a promotion takes effect on kept-alive client connections at once.
//! Streaming bodies (SSE) and upgrades (WebSocket) pass through. Every answer
//! says which build served it (`x-devstack-build`) and whether a newer one is
//! on its way (`x-devstack-pending`). Without an upstream the proxy either
//! holds requests for a while (the service is starting) or refuses them with
//! the reason (it is stopped); it never answers for the service.
//!
//! The conditions of ADR-0010: health checks never pass through here; the
//! proxy's state is in the API ([`ProxyView`]); services can opt out
//! (`proxy = false`); long-lived connections are drained with a deadline and
//! then closed by the proxy, so the old instance can stop at once.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

type Body = BoxBody<Bytes, hyper::Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub port: u16,
    pub build: Option<String>,
}

/// What a request does while there is no upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waiting {
    /// Wait up to this long for an upstream (the service is starting).
    Hold(Duration),
    /// Answer 503 at once with this reason (the service is stopped).
    Refuse(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyView {
    pub port: u16,
    pub upstream: Option<u16>,
    pub build: Option<String>,
    pub pending: Option<String>,
    /// Requests and streams in flight, all upstreams.
    pub active: usize,
    pub served: u64,
    pub last_error: Option<String>,
}

struct Track {
    active: AtomicUsize,
    abort: watch::Sender<bool>,
}

struct Shared {
    name: String,
    port: u16,
    upstream: watch::Sender<Option<Upstream>>,
    waiting: Mutex<Waiting>,
    pending: Mutex<Option<String>>,
    tracks: Mutex<HashMap<u16, Arc<Track>>>,
    served: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// `true` ends the listeners (a removed lane's proxies, `Proxy::close`).
    closed: watch::Sender<bool>,
}

impl Shared {
    fn track(&self, port: u16) -> Arc<Track> {
        self.tracks
            .lock()
            .unwrap()
            .entry(port)
            .or_insert_with(|| {
                Arc::new(Track {
                    active: AtomicUsize::new(0),
                    abort: watch::channel(false).0,
                })
            })
            .clone()
    }
}

/// Decrements the upstream's count when a request, its body or its upgraded
/// connection is done.
struct Active(Arc<Track>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A running proxy. Dropping it does not stop the listener (it lives as long
/// as the daemon); `close` does.
#[derive(Clone)]
pub struct Proxy {
    shared: Arc<Shared>,
}

impl Proxy {
    /// Listen on the fixed port, on IPv4 loopback and, where available, IPv6
    /// loopback (a client may resolve `localhost` to `::1` first).
    pub fn bind(name: &str, port: u16, rt: &tokio::runtime::Handle) -> std::io::Result<Self> {
        let v4 = std::net::TcpListener::bind(("127.0.0.1", port))?;
        let v6 = std::net::TcpListener::bind(("::1", port)).ok();
        let shared = Arc::new(Shared {
            name: name.into(),
            port,
            upstream: watch::channel(None).0,
            waiting: Mutex::new(Waiting::Refuse(format!("`{name}` is not started"))),
            pending: Mutex::new(None),
            tracks: Mutex::new(HashMap::new()),
            served: AtomicU64::new(0),
            last_error: Mutex::new(None),
            closed: watch::channel(false).0,
        });
        for l in std::iter::once(v4).chain(v6) {
            l.set_nonblocking(true)?;
            let sh = shared.clone();
            let mut closed = shared.closed.subscribe();
            rt.spawn(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(l) else {
                    return;
                };
                loop {
                    let accepted = tokio::select! {
                        a = listener.accept() => a,
                        _ = closed.wait_for(|c| *c) => return,
                    };
                    let Ok((stream, _)) = accepted else {
                        // A connection reset before it was accepted, or no
                        // file handles left: keep listening, without spinning.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    };
                    let sh = sh.clone();
                    tokio::spawn(async move {
                        let service = hyper::service::service_fn(move |r| handle(sh.clone(), r));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .with_upgrades()
                            .await;
                    });
                }
            });
        }
        Ok(Self { shared })
    }

    /// Stop listening (the port is free once the listener tasks have ended,
    /// a moment later); open connections end on their own.
    pub fn close(&self) {
        self.shared.closed.send_replace(true);
    }

    pub fn set_upstream(&self, upstream: Option<Upstream>) {
        self.shared.upstream.send_replace(upstream);
    }

    pub fn upstream(&self) -> Option<Upstream> {
        self.shared.upstream.borrow().clone()
    }

    pub fn set_waiting(&self, waiting: Waiting) {
        *self.shared.waiting.lock().unwrap() = waiting;
    }

    pub fn set_pending(&self, pending: Option<String>) {
        *self.shared.pending.lock().unwrap() = pending;
    }

    pub fn active(&self, port: u16) -> usize {
        self.shared.track(port).active.load(Ordering::SeqCst)
    }

    /// Let requests to `port` finish for up to `within`, then close what is
    /// left (streams, WebSockets). Returns the number closed. Blocking.
    pub fn drain(&self, port: u16, within: Duration) -> usize {
        let track = self.shared.track(port);
        let t0 = Instant::now();
        while track.active.load(Ordering::SeqCst) > 0 && t0.elapsed() < within {
            std::thread::sleep(Duration::from_millis(25));
        }
        let left = track.active.load(Ordering::SeqCst);
        if left > 0 {
            track.abort.send_replace(true);
            let t1 = Instant::now();
            while track.active.load(Ordering::SeqCst) > 0 && t1.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.shared.tracks.lock().unwrap().remove(&port);
        left
    }

    pub fn view(&self) -> ProxyView {
        let up = self.upstream();
        let active = self
            .shared
            .tracks
            .lock()
            .unwrap()
            .values()
            .map(|t| t.active.load(Ordering::SeqCst))
            .sum();
        ProxyView {
            port: self.shared.port,
            upstream: up.as_ref().map(|u| u.port),
            build: up.and_then(|u| u.build),
            pending: self.shared.pending.lock().unwrap().clone(),
            active,
            served: self.shared.served.load(Ordering::SeqCst),
            last_error: self.shared.last_error.lock().unwrap().clone(),
        }
    }
}

fn text(status: StatusCode, state: &str, body: String) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(body)).map_err(|n| match n {}).boxed());
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    if let Ok(v) = state.parse() {
        h.insert("x-devstack-state", v);
    }
    r
}

async fn wait_upstream(sh: &Shared) -> Result<Upstream, (String, String)> {
    let mut rx = sh.upstream.subscribe();
    let t0 = Instant::now();
    loop {
        if let Some(u) = rx.borrow_and_update().clone() {
            return Ok(u);
        }
        let waiting = sh.waiting.lock().unwrap().clone();
        match waiting {
            Waiting::Refuse(reason) => return Err(("stopped".into(), reason)),
            Waiting::Hold(max) => {
                let Some(left) = max.checked_sub(t0.elapsed()) else {
                    return Err((
                        "starting".into(),
                        format!("`{}` is still starting after {} s", sh.name, max.as_secs()),
                    ));
                };
                // Wake on a new upstream, or recheck the mode now and then.
                let _ =
                    tokio::time::timeout(left.min(Duration::from_millis(500)), rx.changed()).await;
            }
        }
    }
}

/// Resolves once the drain abort for this upstream is set (never, if its
/// sender is gone). Holds no lock across the await, so the future is `Send`.
async fn aborted(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn connect(port: u16) -> std::io::Result<TcpStream> {
    match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => Ok(s),
        Err(e) => TcpStream::connect(("::1", port)).await.map_err(|_| e),
    }
}

async fn handle(sh: Arc<Shared>, mut req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let up = match wait_upstream(&sh).await {
        Ok(u) => u,
        Err((state, reason)) => {
            return Ok(text(
                StatusCode::SERVICE_UNAVAILABLE,
                &state,
                format!("devstack: {reason}\n"),
            ));
        }
    };
    let track = sh.track(up.port);
    track.active.fetch_add(1, Ordering::SeqCst);
    let guard = Active(track.clone());
    let fail = |e: String| {
        *sh.last_error.lock().unwrap() = Some(e.clone());
        Ok(text(
            StatusCode::BAD_GATEWAY,
            "upstream-error",
            format!("devstack: `{}` (port {}): {e}\n", sh.name, up.port),
        ))
    };

    let stream = match connect(up.port).await {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(x) => x,
        Err(e) => return fail(e.to_string()),
    };
    tokio::spawn(conn.with_upgrades());

    let client_upgrade = req
        .headers()
        .contains_key(hyper::header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    let mut resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => return fail(e.to_string()),
    };
    sh.served.fetch_add(1, Ordering::Relaxed);
    {
        let h = resp.headers_mut();
        if let Some(b) = up.build.as_deref().and_then(|b| b.parse().ok()) {
            h.insert("x-devstack-build", b);
        }
        if let Some(p) = sh
            .pending
            .lock()
            .unwrap()
            .as_deref()
            .and_then(|p| p.parse().ok())
        {
            h.insert("x-devstack-pending", p);
        }
    }
    let mut abort = track.abort.subscribe();

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            let _guard = guard;
            if let (Ok(c), Ok(u)) = (client_upgrade.await, upstream_upgrade.await) {
                let (mut c, mut u) = (TokioIo::new(c), TokioIo::new(u));
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut c, &mut u) => {}
                    _ = aborted(&mut abort) => {}
                }
            }
        });
        let (parts, _) = resp.into_parts();
        return Ok(Response::from_parts(
            parts,
            Full::new(Bytes::new()).map_err(|n| match n {}).boxed(),
        ));
    }

    // Stream the body through a task that also listens for the drain abort.
    let (parts, mut body) = resp.into_parts();
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
    tokio::spawn(async move {
        let _guard = guard;
        loop {
            tokio::select! {
                frame = body.frame() => match frame {
                    Some(f) => if tx.send(f).await.is_err() { break },
                    None => break,
                },
                _ = aborted(&mut abort) => break,
            }
        }
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Ok(Response::from_parts(parts, StreamBody::new(stream).boxed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    /// A tiny upstream: `/` answers its name, `/stream` streams lines until closed.
    fn upstream(rt: &tokio::runtime::Runtime, name: &'static str) -> u16 {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        l.set_nonblocking(true).unwrap();
        rt.spawn(async move {
            let l = tokio::net::TcpListener::from_std(l).unwrap();
            loop {
                let (s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |r: Request<Incoming>| async move {
                        if r.uri().path() == "/stream" {
                            let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(4);
                            tokio::spawn(async move {
                                for i in 0.. {
                                    let line = Bytes::from(format!("data: {name} {i}\n\n"));
                                    if tx.send(Ok(Frame::data(line))).await.is_err() {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(50)).await;
                                }
                            });
                            let body =
                                StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(rx));
                            return Ok::<_, Infallible>(Response::new(body.boxed()));
                        }
                        Ok(Response::new(Full::new(Bytes::from(name)).boxed()))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(s), svc)
                        .await;
                });
            }
        });
        port
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// One request on a kept-alive connection; returns (status, headers, body).
    fn get(conn: &mut std::net::TcpStream, path: &str) -> (u16, String, String) {
        write!(conn, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut r = BufReader::new(conn.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let status = head[9..12].parse().unwrap();
        let lower = head.to_ascii_lowercase();
        let body = if let Some(i) = lower.find("content-length: ") {
            let n: usize = lower[i + 16..]
                .split("\r\n")
                .next()
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let mut b = vec![0; n];
            r.read_exact(&mut b).unwrap();
            String::from_utf8(b).unwrap()
        } else {
            // chunked: one chunk is enough for these tests
            let mut size = String::new();
            r.read_line(&mut size).unwrap();
            let n = usize::from_str_radix(size.trim(), 16).unwrap();
            let mut b = vec![0; n + 2];
            r.read_exact(&mut b).unwrap();
            let mut end = String::new();
            r.read_line(&mut end).unwrap();
            r.read_line(&mut end).unwrap();
            String::from_utf8_lossy(&b[..n]).into_owned()
        };
        (status, lower, body)
    }

    #[test]
    fn flips_on_a_kept_alive_connection_and_tags_the_build() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (a, b) = (upstream(&rt, "a"), upstream(&rt, "b"));
        let port = free_port();
        let p = Proxy::bind("api", port, rt.handle()).unwrap();
        p.set_upstream(Some(Upstream {
            port: a,
            build: Some("b1".into()),
        }));
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (s, h, body) = get(&mut c, "/");
        assert_eq!((s, body.as_str()), (200, "a"));
        assert!(h.contains("x-devstack-build: b1"), "{h}");
        p.set_upstream(Some(Upstream {
            port: b,
            build: Some("b2".into()),
        }));
        p.set_pending(Some("building".into()));
        let (_, h, body) = get(&mut c, "/");
        assert_eq!(
            body, "b",
            "the next request on the same connection goes to the new upstream"
        );
        assert!(
            h.contains("x-devstack-build: b2") && h.contains("x-devstack-pending: building"),
            "{h}"
        );
    }

    #[test]
    fn holds_while_starting_and_refuses_when_stopped() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let a = upstream(&rt, "a");
        let port = free_port();
        let p = Proxy::bind("api", port, rt.handle()).unwrap();

        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (s, h, body) = get(&mut c, "/");
        assert_eq!(s, 503);
        assert!(
            h.contains("x-devstack-state: stopped") && body.contains("not started"),
            "{h}{body}"
        );

        p.set_waiting(Waiting::Hold(Duration::from_secs(5)));
        let p2 = p.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            p2.set_upstream(Some(Upstream {
                port: a,
                build: None,
            }));
        });
        let t0 = Instant::now();
        let (s, _, body) = get(&mut c, "/");
        assert_eq!((s, body.as_str()), (200, "a"), "held, then served");
        assert!(t0.elapsed() >= Duration::from_millis(250));
        t.join().unwrap();
    }

    #[test]
    fn drain_closes_a_stream_after_the_deadline() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (a, b) = (upstream(&rt, "a"), upstream(&rt, "b"));
        let port = free_port();
        let p = Proxy::bind("api", port, rt.handle()).unwrap();
        p.set_upstream(Some(Upstream {
            port: a,
            build: None,
        }));

        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET /stream HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        while !line.contains("data: a") {
            line.clear();
            r.read_line(&mut line).unwrap();
        }
        assert_eq!(p.active(a), 1);
        p.set_upstream(Some(Upstream {
            port: b,
            build: None,
        }));
        let t0 = Instant::now();
        assert_eq!(
            p.drain(a, Duration::from_millis(300)),
            1,
            "one stream left at the deadline"
        );
        assert!(t0.elapsed() >= Duration::from_millis(300));
        // The client sees the end of the chunked body (the final zero chunk).
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut ended = false;
        for _ in 0..200 {
            line.clear();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line == "0\r\n" {
                ended = true;
                break;
            }
        }
        assert!(ended, "the stream was closed for the client");
        assert_eq!(p.active(a), 0);
    }
}
