import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'proxy.rs',[
("""/// A kept connection to `port` that can take a request, or a new one.
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
    let stream""",
"""/// A kept connection to `port` that can take a request, if there is one.
fn kept(sh: &Shared, port: u16) -> Option<Sender> {
    loop {
        let kept = sh.pool.lock().unwrap().get_mut(&port).and_then(Vec::pop)?;
        if kept.is_ready() {
            return Some(kept);
        }
        // closed meanwhile: next
    }
}

/// A new connection to `port`.
async fn sender(port: u16) -> Result<Sender, String> {
    let stream"""),
("""    let mut sender = match sender(&sh, up.port).await {
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
    };""",
"""    let client_upgrade = req
        .headers()
        .contains_key(hyper::header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    strip_hop_by_hop(req.headers_mut(), client_upgrade.is_some());
    // A kept connection the upstream closed while it was idle gives the
    // request back unsent: it goes out on a new one.
    let mut req = Some(req);
    let mut pooled = kept(&sh, up.port);
    let (mut sender, mut resp) = loop {
        let fresh = pooled.is_none();
        let mut s = match pooled.take() {
            Some(s) => s,
            None => match sender(up.port).await {
                Ok(s) => s,
                Err(e) => return fail(e),
            },
        };
        match s.try_send_request(req.take().expect("a request to send")).await {
            Ok(r) => break (s, r),
            Err(mut e) => match e.take_message() {
                Some(back) if !fresh => req = Some(back),
                _ => return fail(e.into_error().to_string()),
            },
        }
    };"""),
])
print("ok")
