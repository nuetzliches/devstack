//! Kept HTTP/1.1 connections to upstreams, per key (#556). A proxy that
//! opens a connection per request and closes it first leaves each one in
//! `TIME_WAIT` on its side for two minutes; under the load of a test suite
//! that drains the ephemeral ports (Windows has 16384). A connection goes
//! back once its answer is read to its end ([`GiveBack`]); one the browser
//! leaves early, or an upgraded one, does not.
//!
//! Used by the static server's forwards and the hub's way to a daemon. The
//! service proxy (`devstack-daemon`, `proxy.rs`) keeps its own, tied to the
//! drain of an instance it switches away from.

use bytes::Bytes;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::{Request, Response};
use std::collections::HashMap;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// Idle connections kept per key, unless a pool says otherwise.
pub const IDLE_MAX: usize = 32;

pub struct Pool<K, B> {
    idle: Mutex<HashMap<K, Vec<SendRequest<B>>>>,
    max: usize,
}

impl<K, B> std::fmt::Debug for Pool<K, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").field("max", &self.max).finish()
    }
}

impl<K, B> Pool<K, B>
where
    K: Eq + Hash + Clone + Send + 'static,
    B: Body + Send + 'static,
{
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            idle: Mutex::new(HashMap::new()),
            max,
        })
    }

    /// An idle connection for `key` that can take a request, if there is one.
    pub fn kept(&self, key: &K) -> Option<SendRequest<B>> {
        let mut idle = self.idle.lock().unwrap();
        let list = idle.get_mut(key)?;
        while let Some(s) = list.pop() {
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
        if list.len() < self.max {
            list.push(sender);
        }
    }

    /// How many idle connections `key` has (for tests and views).
    pub fn idle(&self, key: &K) -> usize {
        self.idle.lock().unwrap().get(key).map_or(0, Vec::len)
    }

    /// Send `req` on a kept connection for `key`, else on one `connect`
    /// opens. A kept one the upstream closed while it was idle hands the
    /// request back unsent; it goes out on a new one.
    pub async fn send<F, Fut>(
        &self,
        key: &K,
        req: Request<B>,
        connect: F,
    ) -> Result<(SendRequest<B>, Response<Incoming>), String>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<SendRequest<B>, String>>,
    {
        let mut req = Some(req);
        let mut pooled = self.kept(key);
        loop {
            let fresh = pooled.is_none();
            let mut s = match pooled.take() {
                Some(s) => s,
                None => connect().await?,
            };
            match s
                .try_send_request(req.take().expect("a request to send"))
                .await
            {
                Ok(r) => return Ok((s, r)),
                Err(mut e) => match e.take_message() {
                    Some(back) if !fresh => req = Some(back),
                    _ => return Err(e.into_error().to_string()),
                },
            }
        }
    }
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
