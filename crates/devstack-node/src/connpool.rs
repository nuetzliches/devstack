//! Kept HTTP/1.1 connections to upstreams, per key (#556). A proxy that
//! opens a connection per request and closes it first leaves each one in
//! `TIME_WAIT` on its side for two minutes; under the load of a test suite
//! that drains the ephemeral ports (Windows has 16384). A connection goes
//! back once its answer is read to its end ([`GiveBack`]); one the browser
//! leaves early, or an upgraded one, does not.
//!
//! Used by the static server's forwards, the public link router, the hub's
//! way to a daemon and the service proxy (`devstack-daemon`, `proxy.rs`),
//! which also forgets an instance's connections when it drains it.
//!
//! An upstream closes a connection that sat idle for its keep-alive timeout
//! (Node.js 5 s, gunicorn 2 s, Kestrel 130 s). A request written on it just
//! as the close arrives fails although the upstream never saw it: hyper
//! hands it back unsent only if it noticed the close first, otherwise it
//! reports the connection closed before an answer. [`Pool::send`] sends such
//! a request once more on a new connection when that is safe ([`replay`]),
//! and a connection idle for longer than [`IDLE_FOR`] is not reused at all.

use bytes::Bytes;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::{Method, Request, Response};
use std::collections::HashMap;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Idle connections kept per key, unless a pool says otherwise.
pub const IDLE_MAX: usize = 32;

/// How long a connection may sit idle and still take the next request.
/// Below the 5 s keep-alive of Node.js (Vite, Express, Next.js), the most
/// common upstream in a stack, so the race with its close is rare; the
/// retry in [`Pool::send`] covers the rest (gunicorn closes after 2 s, a
/// server may close for its own reasons). Not lower: a connection dropped
/// here leaves its `TIME_WAIT` on this side, and that costs nothing at one
/// per four idle seconds, while a connection busy under load (the case of
/// #556) never sits idle that long. An upstream that keeps connections
/// longer (Kestrel 130 s, nginx 75 s) only gets a new one after a pause.
pub const IDLE_FOR: Duration = Duration::from_secs(4);

/// An idle connection and since when it is idle.
type Idle<B> = (SendRequest<B>, Instant);

pub struct Pool<K, B> {
    /// Per key, the idle connections, the newest last.
    idle: Mutex<HashMap<K, Vec<Idle<B>>>>,
    max: usize,
    idle_for: Duration,
}

impl<K, B> std::fmt::Debug for Pool<K, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("max", &self.max)
            .field("idle_for", &self.idle_for)
            .finish()
    }
}

impl<K, B> Pool<K, B>
where
    K: Eq + Hash + Clone + Send + 'static,
    B: Body + Send + 'static,
{
    pub fn new(max: usize) -> Arc<Self> {
        Self::idle_for(max, IDLE_FOR)
    }

    /// A pool whose connections are not reused after `idle_for` idle.
    pub fn idle_for(max: usize, idle_for: Duration) -> Arc<Self> {
        Arc::new(Self {
            idle: Mutex::new(HashMap::new()),
            max,
            idle_for,
        })
    }

    /// An idle connection for `key` that can take a request, if there is one.
    pub fn kept(&self, key: &K) -> Option<SendRequest<B>> {
        let mut idle = self.idle.lock().unwrap();
        let list = idle.get_mut(key)?;
        while let Some((s, since)) = list.pop() {
            if since.elapsed() >= self.idle_for {
                // The newest is too old, so are the others: all close.
                list.clear();
                return None;
            }
            if s.is_ready() {
                return Some(s);
            }
            // closed meanwhile: next
        }
        None
    }

    /// Keep `sender` for `key` once its connection is idle again, unless it
    /// closes or `max` are kept already.
    pub async fn give_back(&self, key: K, mut sender: SendRequest<B>) {
        if sender.ready().await.is_err() {
            return;
        }
        let mut idle = self.idle.lock().unwrap();
        let list = idle.entry(key).or_default();
        list.retain(|(_, since)| since.elapsed() < self.idle_for);
        if list.len() < self.max {
            list.push((sender, Instant::now()));
        }
    }

    /// Close the idle connections of `key` (an upstream that goes away).
    pub fn forget(&self, key: &K) {
        self.idle.lock().unwrap().remove(key);
    }

    /// How many idle connections `key` has that may still be reused (for
    /// tests and views).
    pub fn idle(&self, key: &K) -> usize {
        self.idle.lock().unwrap().get(key).map_or(0, |l| {
            l.iter()
                .filter(|(_, since)| since.elapsed() < self.idle_for)
                .count()
        })
    }

    /// Send `req` on a kept connection for `key`, else on one `connect`
    /// opens. A kept one the upstream closed while it was idle fails the
    /// request without an answer; it goes out once more on a new connection
    /// if hyper hands it back unsent, or if it can be sent twice
    /// ([`replay`]) and the error is that close ([`closed_unanswered`]).
    /// A new connection is never tried twice: its failure is the upstream's.
    pub async fn send<F, Fut>(
        &self,
        key: &K,
        req: Request<B>,
        connect: F,
    ) -> Result<(SendRequest<B>, Response<Incoming>), String>
    where
        B: Default,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<SendRequest<B>, String>>,
    {
        let req = match self.kept(key) {
            None => req,
            Some(mut s) => {
                let copy = replay(&req);
                match s.try_send_request(req).await {
                    Ok(r) => return Ok((s, r)),
                    Err(e) => match again(e, copy) {
                        Ok(req) => req,
                        Err(e) => return Err(e.to_string()),
                    },
                }
            }
        };
        let mut s = connect().await?;
        let r = s.send_request(req).await.map_err(|e| e.to_string())?;
        Ok((s, r))
    }
}

/// What to send again on a new connection after a request on a kept one
/// failed with `e`: the request hyper hands back unsent, else `copy` (from
/// [`replay`]) if the upstream closed the connection before it answered;
/// otherwise the error.
fn again<B>(
    mut e: hyper::client::conn::TrySendError<Request<B>>,
    copy: Option<Request<B>>,
) -> Result<Request<B>, hyper::Error> {
    if let Some(back) = e.take_message() {
        return Ok(back);
    }
    match copy {
        Some(copy) if closed_unanswered(e.error()) => Ok(copy),
        _ => Err(e.into_error()),
    }
}

/// A copy of `req` to send again if the first is lost to a closed kept
/// connection: only for a method that is idempotent (RFC 9110 §9.2.2), and
/// a request without a body. Its extensions are not copied. As browsers and
/// Go's `net/http` do: the upstream may have seen the first one, which an
/// idempotent request allows; one with a body is not kept to be sent twice.
pub fn replay<B: Body + Default>(req: &Request<B>) -> Option<Request<B>> {
    let idempotent = matches!(
        *req.method(),
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE | Method::PUT | Method::DELETE
    );
    if !idempotent || req.body().size_hint().exact() != Some(0) {
        return None;
    }
    let mut copy = Request::new(B::default());
    *copy.method_mut() = req.method().clone();
    *copy.uri_mut() = req.uri().clone();
    *copy.version_mut() = req.version();
    *copy.headers_mut() = req.headers().clone();
    Some(copy)
}

/// Whether `e` is the upstream closing the connection before any answer:
/// hyper's "connection closed before message completed", or the reset a
/// request written on a closed connection meets (on Windows an abort).
pub fn closed_unanswered(e: &hyper::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset};
    if e.is_incomplete_message() {
        return true;
    }
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            return matches!(io.kind(), ConnectionReset | ConnectionAborted | BrokenPipe);
        }
        source = s.source();
    }
    false
}

/// The body of an answer that gives its connection back to the pool once it
/// is read to its end. Dropped before that (the browser went away, a stream
/// that does not end), the connection goes with it.
pub struct GiveBack<K, B> {
    body: Incoming,
    back: Option<Back<K, B>>,
}

/// Where a connection goes at the end of its answer.
type Back<K, B> = (Arc<Pool<K, B>>, K, SendRequest<B>);

impl<K, B> std::fmt::Debug for GiveBack<K, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GiveBack")
            .field("body", &self.body)
            .field("pending", &self.back.is_some())
            .finish()
    }
}

impl<K, B> GiveBack<K, B>
where
    K: Eq + Hash + Clone + Send + 'static,
    B: Body + Send + 'static,
{
    /// `resp`'s body, giving `sender` back to `pool` under `key` at its end.
    pub fn wrap(
        pool: Arc<Pool<K, B>>,
        key: K,
        sender: SendRequest<B>,
        resp: Response<Incoming>,
    ) -> Response<Self> {
        let (parts, body) = resp.into_parts();
        let mut wrapped = Self {
            body,
            back: Some((pool, key, sender)),
        };
        // An empty answer (a 204, a HEAD) may never be polled.
        if wrapped.body.is_end_stream() {
            wrapped.done();
        }
        Response::from_parts(parts, wrapped)
    }

    fn done(&mut self) {
        if let Some((pool, key, sender)) = self.back.take() {
            tokio::spawn(async move { pool.give_back(key, sender).await });
        }
    }
}

impl<K, B> Body for GiveBack<K, B>
where
    K: Eq + Hash + Clone + Send + Unpin + 'static,
    B: Body + Send + 'static,
{
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.body).poll_frame(cx);
        match &r {
            // A body of known length is not polled after its last frame:
            // its end is `is_end_stream` then, not a `None`.
            Poll::Ready(None) => this.done(),
            Poll::Ready(Some(Ok(_))) if this.body.is_end_stream() => this.done(),
            Poll::Ready(Some(Err(_))) => this.back = None, // broken: not reused
            _ => {}
        }
        r
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Full};
    use hyper_util::rt::TokioIo;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type B = Full<Bytes>;

    /// How the test upstream treats the requests on one connection.
    #[derive(Clone, Copy)]
    enum Upstream {
        /// Answers the first, then closes on the next without an answer:
        /// what a request meets that is written as the upstream's keep-alive
        /// timeout closes the connection.
        ClosesKept,
        /// Closes on every request without an answer.
        ClosesAll,
    }

    /// A raw HTTP/1.1 upstream; counts the connections it accepted.
    async fn upstream(mode: Upstream) -> (u16, Arc<AtomicUsize>) {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = l.local_addr().unwrap().port();
        let conns = Arc::new(AtomicUsize::new(0));
        let c = conns.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                c.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut served = 0;
                    loop {
                        // One request head (the tests' bodies are tiny).
                        let mut head = Vec::new();
                        let mut b = [0u8; 1024];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut b).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&b[..n]),
                            }
                        }
                        let close = match mode {
                            Upstream::ClosesKept => served > 0,
                            Upstream::ClosesAll => true,
                        };
                        if close {
                            return; // dropped: closed, no answer
                        }
                        served += 1;
                        let ok = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
                        if s.write_all(ok).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (port, conns)
    }

    async fn connect(port: u16) -> Result<SendRequest<B>, String> {
        let s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(s))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        Ok(sender)
    }

    fn request(method: &str, body: &'static str) -> Request<B> {
        Request::builder()
            .method(method)
            .uri("/")
            .header("host", "127.0.0.1")
            .body(Full::new(Bytes::from_static(body.as_bytes())))
            .unwrap()
    }

    /// A request through `pool`, its answer read to its end; the status or
    /// the error the proxy would turn into a 502.
    async fn through(pool: &Arc<Pool<(), B>>, port: u16, req: Request<B>) -> Result<u16, String> {
        let (sender, resp) = pool.send(&(), req, || connect(port)).await?;
        let status = resp.status().as_u16();
        let body = GiveBack::wrap(pool.clone(), (), sender, resp);
        body.into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?;
        Ok(status)
    }

    /// Wait until `pool` keeps a connection again (it goes back in a task).
    async fn kept_again(pool: &Arc<Pool<(), B>>) {
        for _ in 0..200 {
            if pool.idle(&()) == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the connection did not go back to the pool");
    }

    #[tokio::test]
    async fn a_kept_connection_the_upstream_closes_is_retried_on_a_new_one() {
        let (port, conns) = upstream(Upstream::ClosesKept).await;
        let pool = Pool::new(IDLE_MAX);
        assert_eq!(through(&pool, port, request("GET", "")).await, Ok(200));
        kept_again(&pool).await;
        // Written on the kept connection, met by the upstream's close.
        assert_eq!(through(&pool, port, request("GET", "")).await, Ok(200));
        assert_eq!(conns.load(Ordering::SeqCst), 2);
        // Any idempotent method without a body.
        for (n, method) in ["OPTIONS", "PUT", "DELETE"].into_iter().enumerate() {
            kept_again(&pool).await;
            assert_eq!(through(&pool, port, request(method, "")).await, Ok(200));
            assert_eq!(conns.load(Ordering::SeqCst), 3 + n, "{method}");
        }
    }

    #[tokio::test]
    async fn a_request_with_a_body_or_not_idempotent_is_not_sent_twice() {
        for (method, body) in [("POST", "x"), ("POST", ""), ("PUT", "x")] {
            let (port, conns) = upstream(Upstream::ClosesKept).await;
            let pool = Pool::new(IDLE_MAX);
            assert_eq!(through(&pool, port, request("GET", "")).await, Ok(200));
            kept_again(&pool).await;
            let e = through(&pool, port, request(method, body)).await;
            assert!(e.is_err(), "{method} {body:?}: {e:?}");
            assert_eq!(conns.load(Ordering::SeqCst), 1, "{method} {body:?}");
        }
    }

    #[tokio::test]
    async fn a_request_on_a_new_connection_is_not_sent_twice() {
        let (port, conns) = upstream(Upstream::ClosesAll).await;
        let pool = Pool::new(IDLE_MAX);
        let e = through(&pool, port, request("GET", "")).await;
        assert!(e.is_err(), "{e:?}");
        assert_eq!(conns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_connection_idle_past_its_bound_is_not_reused() {
        let (port, conns) = upstream(Upstream::ClosesKept).await;
        let pool = Pool::idle_for(IDLE_MAX, Duration::from_millis(200));
        assert_eq!(through(&pool, port, request("GET", "")).await, Ok(200));
        kept_again(&pool).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(pool.idle(&()), 0);
        // Not written on the old connection: the upstream sees no request
        // there, and the answer comes from a new one.
        assert_eq!(through(&pool, port, request("POST", "x")).await, Ok(200));
        assert_eq!(conns.load(Ordering::SeqCst), 2);
    }
}
