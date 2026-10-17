import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
p=W+'remote.rs'
s=open(p,encoding='utf-8').read()
a=s.index("    /// While the lane runs: sync what changes, renew the lease, read its\n    /// state.\n    async fn follow(")
b=s.index("    /// Listen on `here` (loopback, IPv4 and, where there is one, IPv6) and")
new_follow='''    /// While the lane runs: sync what changes, renew the lease, read its
    /// state. Three loops of their own: a node that does not answer stalls
    /// one of them, not the others (a renewal waits for no sync).
    fn follow(self: &Arc<Self>, rt: &tokio::runtime::Handle, scratch: PathBuf) {
        let me = self.clone();
        rt.spawn(async move { me.follow_sync(scratch).await });
        let me = self.clone();
        rt.spawn(async move { me.follow_renew().await });
        let me = self.clone();
        rt.spawn(async move { me.follow_state().await });
    }

    /// Resolves when the lane is stopped or gone.
    async fn over(&self) {
        let mut stopped = self.stopped.subscribe();
        let mut gone = self.gone.subscribe();
        tokio::select! {
            _ = stopped.wait_for(|s| *s) => {}
            _ = gone.wait_for(|g| *g) => {}
        }
    }

    /// Sleep `d`; `false` when the lane is over meanwhile.
    async fn tick(&self, d: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => true,
            _ = self.over() => false,
        }
    }

    /// Sync what changed in the worktree (ADR-0029 point 2): when the file
    /// watcher saw a change, and in full every [`FULL_SYNC_EVERY`] (what it
    /// does not see: the main tree's `[lanes] sync` files, lost events).
    /// The first look syncs once more: the fingerprint of what the first
    /// sync sent is not known, and the listing is all it costs.
    async fn follow_sync(self: Arc<Self>, scratch: PathBuf) {
        let mut seen: Option<Fingerprint> = None;
        let mut full = Instant::now() - FULL_SYNC_EVERY;
        loop {
            if !self.tick(SYNC_EVERY).await {
                return;
            }
            let changed = self.dirty.swap(false, std::sync::atomic::Ordering::SeqCst);
            if !changed && full.elapsed() < FULL_SYNC_EVERY {
                continue;
            }
            if full.elapsed() >= FULL_SYNC_EVERY {
                full = Instant::now();
            }
            let (wt, main, extra) = (self.worktree.clone(), self.main.clone(), self.extra.clone());
            let sc = scratch.clone();
            let listed = tokio::task::spawn_blocking(move || {
                let (list, elsewhere) = files(&wt, &main, &extra, &sc)?;
                let fp = fingerprint(&wt, &list, &elsewhere);
                Ok::<_, String>((list, elsewhere, fp))
            })
            .await;
            match listed {
                Ok(Ok((list, elsewhere, fp))) => {
                    if seen.as_ref() != Some(&fp) {
                        match self
                            .client
                            .sync_from(
                                &self.lease,
                                &self.worktree,
                                &list,
                                &elsewhere,
                                &|_, _, _| {},
                            )
                            .await
                        {
                            Ok(s) => {
                                if s.written + s.removed > 0 {
                                    crate::diag::info(
                                        "remote_lane_synced",
                                        serde_json::json!({ "lane": self.name, "uploaded": s.uploaded,
                                            "written": s.written, "removed": s.removed }),
                                    );
                                }
                                seen = Some(fp);
                                self.trouble(None);
                            }
                            Err(e) => {
                                // Tried again at the next tick.
                                self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                                self.trouble(Some(format!("sync: {e}")));
                            }
                        }
                    }
                }
                Ok(Err(e)) => self.trouble(Some(format!("listing the worktree: {e}"))),
                Err(_) => {}
            }
        }
    }

    /// The lane is over for this machine: say why, stop listening, end
    /// its tunnels.
    fn end(&self, why: String, event: &str) {
        {
            let mut st = self.state.lock().unwrap();
            st.ended = true;
            st.error = Some(why);
        }
        self.listening.send_replace(false);
        self.gone.send_replace(true);
        crate::diag::warn(
            event,
            serde_json::json!({ "lane": self.name, "node": self.node.name, "lease": self.lease }),
        );
    }

    /// Renew the lease every [`RENEW_EVERY`]; tell the lane again that it
    /// holds the main slot and the coordinator whom it is shared with.
    async fn follow_renew(self: Arc<Self>) {
        let mut renewed = Instant::now();
        loop {
            if !self.tick(RENEW_EVERY).await {
                return;
            }
            match self.client.renew(&self.lease).await {
                Ok(_) => {
                    renewed = Instant::now();
                    if self.owns_main() {
                        let _ = self
                            .client
                            .api(
                                &self.lease,
                                "POST",
                                "/v1/main-slot",
                                Some(serde_json::json!({ "owns": true })),
                            )
                            .await;
                    }
                    let sharing = self.sharing.lock().unwrap().clone();
                    if let Some(sh) = sharing {
                        let (pool, lease) = (self.pool.clone(), self.lease.clone());
                        let _ = tokio::task::spawn_blocking(move || {
                            local_member(&pool).share_lease(&lease, Some(&sh))
                        })
                        .await;
                    }
                }
                Err(devstack_node::nodeclient::ClientError::Status(404, _)) => {
                    return self.end(
                        format!(
                            "the lease on {} is over (an admin ended it, or it expired): \\
                             `devstack up --on {}` starts the lane again",
                            self.node.name, self.node.name
                        ),
                        "remote_lane_ended",
                    );
                }
                Err(e) => {
                    self.trouble(Some(format!("renewing the lease: {e}")));
                    // Not renewed for longer than a lease lives: the node let
                    // it go, or cannot be reached; either way the lane is
                    // gone for this machine.
                    if renewed.elapsed() > LEASE_LIVES {
                        return self.end(
                            format!(
                                "{} has not answered for {} s: the lane counts as gone",
                                self.node.name,
                                renewed.elapsed().as_secs()
                            ),
                            "remote_lane_lost",
                        );
                    }
                }
            }
        }
    }

    /// Read the lane's state for the lists every [`STATE_EVERY`].
    async fn follow_state(self: Arc<Self>) {
        loop {
            if let Ok((200, body)) = self.client.api(&self.lease, "GET", "/v1/stack", None).await
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
            {
                let svcs = v["services"].as_array().cloned().unwrap_or_default();
                let running = svcs
                    .iter()
                    .filter(|s| {
                        matches!(
                            s["status"]["state"].as_str(),
                            Some("healthy" | "starting" | "building" | "running")
                        )
                    })
                    .count();
                let active = svcs
                    .iter()
                    .filter(|s| matches!(s["status"]["state"].as_str(), Some("healthy" | "running")))
                    .filter_map(|s| s["name"].as_str().map(str::to_string))
                    .collect();
                let mut st = self.state.lock().unwrap();
                st.running = running;
                st.services = svcs.len();
                st.active = active;
            }
            if !self.tick(STATE_EVERY).await {
                return;
            }
        }
    }

'''
s=s[:a]+new_follow+s[b:]
open(p,'w',encoding='utf-8',newline='').write(s)

apply(p,[
("""/// How often the worktree is checked for changes to sync.
const SYNC_EVERY: Duration = Duration::from_secs(2);""",
"""/// How often the file watcher's changes are synced (at most).
const SYNC_EVERY: Duration = Duration::from_secs(2);
/// How often everything is compared regardless of the watcher.
const FULL_SYNC_EVERY: Duration = Duration::from_secs(30);"""),
("""    /// Whether this machine listens on them for the lane now: a lane that
    /// moves here takes them over before its old placement goes.
    listening: tokio::sync::watch::Sender<bool>,
}""",
"""    /// Whether this machine listens on them for the lane now: a lane that
    /// moves here takes them over before its old placement goes.
    listening: tokio::sync::watch::Sender<bool>,
    /// The lease is over or lost: its open tunnels end too.
    gone: tokio::sync::watch::Sender<bool>,
    /// The worktree changed since the last sync (set by the file watcher;
    /// always, when there is none).
    dirty: Arc<std::sync::atomic::AtomicBool>,
    /// Watches the worktree while the lane runs.
    _watcher: Option<notify::RecommendedWatcher>,
}"""),
("""/// Files below `root/base`, relative to `root`, `/`-separated.
fn walk(root: &Path, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.join(base)];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() && e.file_name() != ".git" => dirs.push(p),""",
"""/// Directories a `[lanes] sync` pattern never reaches into: git's,
/// devstack's own state (artifacts, snapshots, logs: a `.pem` in a snapshot
/// is no source) and installed packages, the largest trees of a checkout.
const NOT_WALKED: [&str; 3] = [".git", ".devstack", "node_modules"];

/// Files below `root/base`, relative to `root`, `/`-separated.
fn walk(root: &Path, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.join(base)];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() && !NOT_WALKED.iter().any(|n| e.file_name() == *n) => {
                    dirs.push(p)
                }"""),
("""        step(1, &format!("lease on {on}"));
        let (pool, node) = resolve_on(on)?;
        let local = local_member(&pool);
        let offer = local""",
"""        step(1, &format!("lease on {on}"));
        let (pool, node) = resolve_on(on)?;
        let local = local_member(&pool);
        // Listed before the lease: a failure here holds no capacity.
        let (list, elsewhere) = files(worktree, main, extra, scratch)?;
        let offer = local"""),
("""        let client = NodeClient::new(&local, offer.node.clone()).map_err(|e| e.to_string())?;
        let (list, elsewhere) = files(worktree, main, extra, scratch)?;
        let t0 = Instant::now();""",
"""        // Without a client the node cannot be told: the offer expires.
        let client = NodeClient::new(&local, offer.node.clone()).map_err(|e| e.to_string())?;
        let t0 = Instant::now();"""),
("""        let lane = Arc::new(Self {
            name: name.into(),""",
"""        // The worktree's changes are synced when they happen (ADR-0029
        // point 2); without a watcher, every few seconds.
        let dirty = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let watcher = {
            let (dirty, root) = (dirty.clone(), worktree.to_path_buf());
            let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let relevant = match res {
                    Ok(ev) => {
                        ev.need_rescan()
                            || ev.paths.iter().any(|p| {
                                p.strip_prefix(&root).is_ok_and(|r| {
                                    !crate::watch::ignored(&r.to_string_lossy().replace('\\\\', "/"))
                                })
                            })
                    }
                    Err(_) => true,
                };
                if relevant {
                    dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .and_then(|mut w| {
                use notify::Watcher as _;
                w.watch(worktree, notify::RecursiveMode::Recursive).map(|()| w)
            });
            match w {
                Ok(w) => Some(w),
                Err(e) => {
                    crate::diag::warn(
                        "remote_lane_unwatched",
                        serde_json::json!({ "lane": name, "error": e.to_string(),
                            "why": "the worktree is compared every few seconds instead" }),
                    );
                    None
                }
            }
        };
        let lane = Arc::new(Self {
            name: name.into(),"""),
("""            ports,
            listening: tokio::sync::watch::channel(false).0,
        });""",
"""            ports,
            listening: tokio::sync::watch::channel(false).0,
            gone: tokio::sync::watch::channel(false).0,
            dirty: dirty.clone(),
            _watcher: watcher,
        });
        if lane._watcher.is_none() {
            // No watcher: every tick counts as a change.
            let dirty = dirty.clone();
            let mut over = lane.stopped.subscribe();
            rt.spawn(async move {
                loop {
                    dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                    tokio::select! {
                        _ = tokio::time::sleep(SYNC_EVERY) => {}
                        _ = over.wait_for(|s| *s) => return,
                    }
                }
            });
        }"""),
("""        let l = lane.clone();
        let scratch = scratch.to_path_buf();
        if forward {
            lane.start_forwarding(rt);
        }
        // The first look syncs once more: the fingerprint of what the first
        // sync sent is not known, and the listing is all it costs.
        rt.spawn(async move { l.follow(scratch, None).await });
        Ok(lane)""",
"""        if forward {
            lane.start_forwarding(rt);
        }
        lane.follow(rt, scratch.to_path_buf());
        Ok(lane)"""),
("""                    let me = me.clone();
                    tokio::spawn(async move {
                        if let Err(e) = me.tunnel(tcp, there).await {
                            crate::diag::warn(
                                "remote_lane_tunnel",
                                serde_json::json!({ "lane": me.name, "port": there, "error": e }),
                            );
                        }
                    });""",
"""                    let me = me.clone();
                    tokio::spawn(async move {
                        // A lease that is over ends its open connections too
                        // (a dev server's WebSocket would hang until TCP
                        // gives up).
                        let r = tokio::select! {
                            r = me.tunnel(tcp, there) => r,
                            _ = me.over() => Ok(()),
                        };
                        if let Err(e) = r {
                            crate::diag::warn_limited(
                                &format!("remote_lane_tunnel:{}:{there}", me.name),
                                "remote_lane_tunnel",
                                serde_json::json!({ "lane": me.name, "port": there, "error": e }),
                            );
                        }
                    });"""),
("""    /// Listen on `here` (loopback, IPv4 and, where there is one, IPv6) and
    /// carry every connection to port `there` of the lane on its node: bytes
    /// unchanged, so WebSockets, hot module replacement and TLS dev servers
    /// work as they do locally.
    /// Listen on the lane's ports here (again).
    pub fn start_forwarding""",
"""    /// Listen on the lane's ports here (again): each on loopback, IPv4 and,
    /// where there is one, IPv6, every connection carried to its port of the
    /// lane on its node, bytes unchanged, so WebSockets, hot module
    /// replacement and TLS dev servers work as they do locally.
    pub fn start_forwarding"""),
])
print("ok")
