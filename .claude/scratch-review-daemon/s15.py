import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'proxy.rs',[
("""//! (`proxy = false`); long-lived connections are drained with a deadline and
//! then closed by the proxy, so the old instance can stop at once.""",
"""//! (`proxy = false`); long-lived connections are drained with a deadline and
//! then closed by the proxy, so the old instance can stop at once (a body of
//! known length is let finish: cut, it is a network error for the client).
//!
//! Connections to an upstream are kept and reused (no new connection per
//! request), and hop-by-hop headers stay on their hop (RFC 9110 §7.6.1)."""),
("""type Body = BoxBody<Bytes, hyper::Error>;""",
"""type Body = BoxBody<Bytes, hyper::Error>;

/// Idle connections kept per upstream.
const POOL_MAX: usize = 16;

type Sender = hyper::client::conn::http1::SendRequest<Incoming>;"""),
("""    served: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// `true` ends the listeners (a removed lane's proxies, `Proxy::close`).
    closed: watch::Sender<bool>,
}""",
"""    served: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// `true` ends the listeners (a removed lane's proxies, `Proxy::close`).
    closed: watch::Sender<bool>,
    /// Idle connections to each upstream port, for the next request.
    pool: Mutex<HashMap<u16, Vec<Sender>>>,
}"""),
("""            last_error: Mutex::new(None),
            closed: watch::channel(false).0,
        });""",
"""            last_error: Mutex::new(None),
            closed: watch::channel(false).0,
            pool: Mutex::new(HashMap::new()),
        });"""),
("""    pub fn set_upstream(&self, upstream: Option<Upstream>) {
        self.shared.upstream.send_replace(upstream);
    }""",
"""    /// A new upstream starts without the last one's error (ADR-0010
    /// condition 2: the state shown is the state now).
    pub fn set_upstream(&self, upstream: Option<Upstream>) {
        let changed = *self.shared.upstream.borrow() != upstream;
        self.shared.upstream.send_replace(upstream);
        if changed {
            *self.shared.last_error.lock().unwrap() = None;
        }
    }"""),
("""        self.shared.tracks.lock().unwrap().remove(&port);
        left
    }""",
"""        self.shared.tracks.lock().unwrap().remove(&port);
        // Its kept connections close: the instance stops next.
        self.shared.pool.lock().unwrap().remove(&port);
        left
    }"""),
("""async fn connect(port: u16) -> std::io::Result<TcpStream> {""",
"""/// Remove the hop-by-hop headers (RFC 9110 §7.6.1): those `Connection`
/// names and the fixed ones. An upgrade keeps `Connection` and `Upgrade`,
/// which carry it to the next hop.
fn strip_hop_by_hop(h: &mut hyper::HeaderMap, upgrade: bool) {
    let named: Vec<hyper::header::HeaderName> = h
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| hyper::header::HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for n in named {
        if !(upgrade && n == hyper::header::UPGRADE) {
            h.remove(n);
        }
    }
    for n in [
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        h.remove(n);
    }
    if !upgrade {
        h.remove(hyper::header::CONNECTION);
        h.remove(hyper::header::UPGRADE);
    }
}

/// A kept connection to `port` that can take a request, or a new one.
async fn sender(sh: &Shared, port: u16) -> Result<Sender, String> {
    loop {
        let kept = sh
            .pool
            .lock()
            .unwrap()
            .get_mut(&port)
            .and_then(Vec::pop);
        match kept {
            Some(s) if s.is_ready() => return Ok(s),
            Some(_) => continue, // closed meanwhile
            None => break,
        }
    }
    let stream = connect(port).await.map_err(|e| e.to_string())?;
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn.with_upgrades());
    Ok(sender)
}

/// Keep `sender` for the next request to `port`, once its connection is
/// idle again (the response is read to its end), unless it closes.
async fn give_back(sh: &Shared, port: u16, mut sender: Sender) {
    if sender.ready().await.is_err() {
        return;
    }
    // The upstream may have changed meanwhile; a drained port keeps nothing.
    if sh.upstream.borrow().as_ref().is_none_or(|u| u.port != port) {
        return;
    }
    let mut pool = sh.pool.lock().unwrap();
    let kept = pool.entry(port).or_default();
    if kept.len() < POOL_MAX {
        kept.push(sender);
    }
}

async fn connect(port: u16) -> std::io::Result<TcpStream> {"""),
("""    let stream = match connect(up.port).await {
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
        let h = resp.headers_mut();""",
"""    let mut sender = match sender(&sh, up.port).await {
        Ok(s) => s,
        Err(e) => return fail(e),
    };

    let client_upgrade = req
        .headers()
        .contains_key(hyper::header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    strip_hop_by_hop(req.headers_mut(), client_upgrade.is_some());
    let mut resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => return fail(e.to_string()),
    };
    sh.served.fetch_add(1, Ordering::Relaxed);
    if sh.last_error.lock().unwrap().is_some() {
        *sh.last_error.lock().unwrap() = None; // it answers again
    }
    let upgraded = resp.status() == StatusCode::SWITCHING_PROTOCOLS && client_upgrade.is_some();
    strip_hop_by_hop(resp.headers_mut(), upgraded);
    {
        let h = resp.headers_mut();"""),
("""    // Stream the body through a task that also listens for the drain abort.
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
    });""",
"""    // Stream the body through a task that also listens for the drain abort;
    // a body of known length is let finish (cut, it is a network error for
    // the client, not something to retry). A body read to its end leaves its
    // connection for the next request.
    let (parts, mut body) = resp.into_parts();
    let bounded = parts.headers.contains_key(hyper::header::CONTENT_LENGTH);
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
    let port = up.port;
    tokio::spawn(async move {
        let _guard = guard;
        let complete = loop {
            tokio::select! {
                frame = body.frame() => match frame {
                    Some(Ok(f)) => if tx.send(Ok(f)).await.is_err() { break false },
                    Some(Err(e)) => { let _ = tx.send(Err(e)).await; break false },
                    None => break true,
                },
                _ = aborted(&mut abort), if !bounded => break false,
            }
        };
        if complete {
            give_back(&sh, port, sender).await;
        }
    });"""),
])
print("ok")
