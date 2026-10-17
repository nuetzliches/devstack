import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'lanes.rs',[
("""/// A worktree as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
}""",
"""/// A worktree as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
    /// git says its directory is not there (`prunable`).
    prunable: bool,
}

/// How long git may list a worktree whose directory is not there (a network
/// or removable drive gone for a moment) before its lane is forgotten, with
/// its containers and volumes. A worktree removed with git goes at once.
const MISSING_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What lane `number` moves its ports by; `None` beyond 65535.
fn lane_offset(number: u16, step: u16) -> Option<u16> {
    number.checked_mul(step)
}"""),
("""    /// One [`Lanes::sync`] at a time: asking git and applying the answer go
    /// together, or an answer from before a worktree was added could be
    /// applied after a newer one and drop it as gone.
    syncing: Mutex<()>,
}""",
"""    /// One [`Lanes::sync`] at a time: asking git and applying the answer go
    /// together, or an answer from before a worktree was added could be
    /// applied after a newer one and drop it as gone.
    syncing: Mutex<()>,
    /// Lanes whose worktree git still lists but whose directory is not
    /// there, since when ([`MISSING_GRACE`]).
    missing: Mutex<BTreeMap<String, std::time::Instant>>,
}"""),
("""/// `git worktree list --porcelain`: every worktree whose directory exists,
/// the main working tree included. `None` when git does not answer: that is
/// no reason to forget a lane.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\\n\\n") {
        let mut path = None;
        let mut branch = None;
        let mut skip = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "bare" || line.starts_with("prunable") {
                skip = true;
            }
        }
        if let (Some(path), false) = (path, skip) {
            out.push(Worktree { path, branch });
        }
    }
    out
}

fn worktrees(repo: &Path) -> Option<Vec<Worktree>> {
    let text = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
    Some(
        parse_worktrees(&text.replace("\\r\\n", "\\n"))
            .into_iter()
            .filter(|w| w.path.is_dir())
            .collect(),
    )
}""",
"""/// `git worktree list --porcelain`: every worktree but a bare one, the main
/// working tree included.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\\n\\n") {
        let mut path = None;
        let mut branch = None;
        let mut bare = false;
        let mut prunable = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "bare" {
                bare = true;
            } else if line.starts_with("prunable") {
                prunable = true;
            }
        }
        if let (Some(path), false) = (path, bare) {
            out.push(Worktree {
                path,
                branch,
                prunable,
            });
        }
    }
    out
}

/// The worktrees whose directory exists, and the paths of those git still
/// lists without one. `None` when git does not answer: that is no reason
/// to forget a lane.
fn worktrees(repo: &Path) -> Option<(Vec<Worktree>, Vec<PathBuf>)> {
    let text = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let (here, missing): (Vec<Worktree>, Vec<Worktree>) =
        parse_worktrees(&text.replace("\\r\\n", "\\n"))
            .into_iter()
            .partition(|w| !w.prunable && w.path.is_dir());
    Some((here, missing.into_iter().map(|w| w.path).collect()))
}"""),
("""            handover: Mutex::new(Handover::default()),
            syncing: Mutex::new(()),
        });""",
"""            handover: Mutex::new(Handover::default()),
            syncing: Mutex::new(()),
            missing: Mutex::new(BTreeMap::new()),
        });"""),
("""    /// Follow `git worktree list`: a new worktree becomes a lane, a removed
    /// one is forgotten with its state. Without an answer from git nothing
    /// changes.
    pub fn sync(&self) {
        let _one = self.syncing.lock().unwrap();
        let Some(trees) = worktrees(&self.main.repo) else {
            return;
        };""",
"""    /// Follow `git worktree list`: a new worktree becomes a lane, a removed
    /// one is forgotten with its state. Without an answer from git nothing
    /// changes. A worktree git still lists whose directory is not there is
    /// forgotten only after [`MISSING_GRACE`].
    pub fn sync(&self) {
        // Forgetting a lane stops it (and may give the main slot back, which
        // starts services): not while others wait to read the lanes.
        let Some((gone, changed)) = self.sync_known() else {
            return;
        };
        for name in &gone {
            self.forget(name, "its worktree is gone");
        }
        if changed {
            self.announce();
        }
    }

    /// Ask git and apply its answer to `known`: the lanes to forget, and
    /// whether the clients must hear of a change.
    fn sync_known(&self) -> Option<(Vec<String>, bool)> {
        let _one = self.syncing.lock().unwrap();
        let (trees, missing) = worktrees(&self.main.repo)?;"""),
("""        let gone: Vec<String>;
        let changed;
        {
            let mut known = self.known.lock().unwrap();
            let before = known.len();
            gone = known
                .iter()
                .filter(|(_, k)| !trees.iter().any(|w| same_tree(&w.path, &k.entry.path)))
                .map(|(n, _)| n.clone())
                .collect();""",
"""        let mut gone: Vec<String>;
        let changed;
        {
            let mut known = self.known.lock().unwrap();
            let before = known.len();
            gone = known
                .iter()
                .filter(|(_, k)| !trees.iter().any(|w| same_tree(&w.path, &k.entry.path)))
                .map(|(n, _)| n.clone())
                .collect();
            // Listed by git, but not there right now: give it a while.
            let mut since = self.missing.lock().unwrap();
            since.retain(|n, _| gone.contains(n));
            gone.retain(|n| {
                let listed = missing
                    .iter()
                    .any(|p| same_tree(p, &known[n.as_str()].entry.path));
                if !listed {
                    return true;
                }
                let t = since.entry(n.clone()).or_insert_with(|| {
                    crate::diag::warn(
                        "lane_worktree_missing",
                        serde_json::json!({ "lane": n, "path": known[n.as_str()].entry.path,
                            "forgotten_after_s": MISSING_GRACE.as_secs() }),
                    );
                    std::time::Instant::now()
                });
                t.elapsed() >= MISSING_GRACE
            });"""),
("""            changed = added || moved;
        }
        for name in &gone {
            self.forget(name, "its worktree is gone");
        }
        if changed {
            self.announce();
        }
    }""",
"""            changed = added || moved;
        }
        Some((gone, changed))
    }"""),
("""        let step = self.main.stack.lanes.port_step;
        // A lane's containers publish the main tree's ports moved by the same
        // step (ADR-0030): those must be free as well.
        let mut base = ports(&self.main.stack);
        base.extend(self.main.infra_host_ports());
        let used: BTreeSet<u16> = known.values().map(|k| k.entry.number).collect();
        let mut taken: BTreeSet<u16> = base.iter().copied().collect();
        for k in known.values() {
            taken.extend(
                base.iter()
                    .filter_map(|p| p.checked_add(k.entry.number * step)),
            );
        }
        (1..200u16).filter(|n| !used.contains(n)).find(|n| {
            base.iter().all(|p| {
                p.checked_add(n * step).is_some_and(|q| {
                    !taken.contains(&q) && !(probe && crate::supervisor::port_in_use(q))
                })
            })
        })""",
"""        let step = self.main.stack.lanes.port_step;
        // A lane's containers publish the main tree's ports moved by the same
        // step (ADR-0030): those must be free as well.
        let mut base = ports(&self.main.stack);
        base.extend(self.main.infra_host_ports());
        let used: BTreeSet<u16> = known.values().map(|k| k.entry.number).collect();
        let mut taken: BTreeSet<u16> = base.iter().copied().collect();
        for k in known.values() {
            if let Some(off) = lane_offset(k.entry.number, step) {
                taken.extend(base.iter().filter_map(|p| p.checked_add(off)));
            }
        }
        (1..200u16).filter(|n| !used.contains(n)).find(|n| {
            lane_offset(*n, step).is_some_and(|off| {
                base.iter().all(|p| {
                    p.checked_add(off).is_some_and(|q| {
                        !taken.contains(&q) && !(probe && crate::supervisor::port_in_use(q))
                    })
                })
            })
        })"""),
("""        if let Some(l) = self.map.lock().unwrap().remove(name) {
            l.sup.close();
        }
        if let Some(r) = self.remote.lock().unwrap().remove(name) {
            self.stop_remote(r);
        }
        self.remove_infra(name);""",
"""        // Not under the map's lock: stopping its services takes a while, and
        // `GET /v1/lanes` reads the map.
        let lane = self.map.lock().unwrap().remove(name);
        if let Some(l) = lane {
            l.sup.close();
        }
        let remote = self.remote.lock().unwrap().remove(name);
        if let Some(r) = remote {
            self.stop_remote(r);
        }
        self.missing.lock().unwrap().remove(name);
        self.remove_infra(name);"""),
("""    /// The containers, volumes and networks of lane `name` (ADR-0030), also
    /// of a lane not started now: Compose finds a project by its name alone.
    /// Stop and remove the containers of lane `name` here, keeping their""",
"""    /// Stop and remove the containers of lane `name` here, keeping their"""),
("""    fn remove_infra(&self, name: &str) {""",
"""    /// Remove the containers, volumes and networks of lane `name`
    /// (ADR-0030), also of a lane not started now: Compose finds a project
    /// by its name alone.
    fn remove_infra(&self, name: &str) {"""),
("""        // The main stack's step: the free ranges were chosen with it.
        let offset = entry.number * self.main.stack.lanes.port_step;
        let stack = lane_stack(&stack, offset)?;""",
"""        // The main stack's step: the free ranges were chosen with it.
        let offset = lane_offset(entry.number, self.main.stack.lanes.port_step)
            .ok_or_else(|| format!("lane number {} is beyond the port range", entry.number))?;
        let stack = lane_stack(&stack, offset)?;"""),
("""            entry.name.clone(),
            offset,
            self.main.main_owner(),
        )?;""",
"""            entry.name.clone(),
            offset,
            self.main.main_owner(),
            // One build job per daemon (ADR-0020): a lane that goes takes no
            // build server along that the main tree's build uses.
            Some(self.main.build_job()),
        )?;"""),
("""                    number: k.entry.number,
                    port_offset: k.entry.number * self.main.stack.lanes.port_step,""",
"""                    number: k.entry.number,
                    port_offset: lane_offset(k.entry.number, self.main.stack.lanes.port_step)
                        .unwrap_or(0),"""),
("""            let busy = ports(&self.main.stack).iter().any(|p| {
                p.checked_add(entry.number * step)
                    .is_none_or(crate::supervisor::port_in_use)
            });""",
"""            let busy = ports(&self.main.stack).iter().any(|p| {
                lane_offset(entry.number, step)
                    .and_then(|off| p.checked_add(off))
                    .is_none_or(crate::supervisor::port_in_use)
            });"""),
("""        // Its containers too: their memory is what a stopped lane gives back
        // (ADR-0030). Volumes stay for the next start.
        lane.sup.down(true);
        if let Some(l) = self.map.lock().unwrap().remove(name) {
            l.sup.close();
        }""",
"""        // Its containers too: their memory is what a stopped lane gives back
        // (ADR-0030). Volumes stay for the next start.
        lane.sup.down(true);
        let removed = self.map.lock().unwrap().remove(name);
        if let Some(l) = removed {
            l.sup.close();
        }"""),
("""        let offset = self
            .known
            .lock()
            .unwrap()
            .get(&t)
            .map_or(0, |k| k.entry.number * main.stack.lanes.port_step);
        let remote_active = remote.as_ref().map(|r| r.active_now(&self.rt));
        let active = |s: &str| match &remote_active {
            Some(names) => names.iter().any(|n| n == s).then(|| {
                main.stack
                    .services
                    .get(s)
                    .and_then(|svc| svc.port)
                    .map(|p| (p + offset, None))
            })?,""",
"""        let offset = self
            .known
            .lock()
            .unwrap()
            .get(&t)
            .map_or(Some(0), |k| {
                lane_offset(k.entry.number, main.stack.lanes.port_step)
            })
            .ok_or_else(|| format!("lane `{t}`: its ports are beyond 65535"))?;
        let remote_active = remote.as_ref().map(|r| r.active_now(&self.rt));
        let active = |s: &str| match &remote_active {
            Some(names) => names.iter().any(|n| n == s).then(|| {
                main.stack
                    .services
                    .get(s)
                    .and_then(|svc| svc.port)
                    .and_then(|p| p.checked_add(offset))
                    .map(|p| (p, None))
            })?,"""),
("""        // 1. Free the fixed ports. Forwards to the previous lane end; the main
        //    tree stops what the lane takes over, and remembers it.
        for s in main.forwarded_services() {
            let _ = main.tcp_forward(&s, None);
        }
        if current.is_none() {
            for s in main.proxied_services() {
                if main.service_running(&s) {
                    ho.stopped.insert(s.clone());
                    let _ = main.stop(&s);
                }
            }
        }
        let mut back_home = Vec::new();
        for s in main.unproxied_services() {
            if active(&s).is_some() {
                if main.service_running(&s) {
                    ho.stopped.insert(s.clone());
                    let _ = main.stop(&s);
                }""",
"""        // 1. Free the fixed ports. Forwards to the previous lane end; the main
        //    tree stops what the lane takes over, and remembers it. From now
        //    on nothing of the main tree takes a proxy's upstream: a start in
        //    flight (building, waiting for health) is stopped under its op
        //    lock, and one that comes later is refused (ADR-0008).
        let handing = Handing(main);
        main.set_handing_over(true);
        for s in main.forwarded_services() {
            let _ = main.tcp_forward(&s, None);
        }
        if current.is_none() {
            for s in main.proxied_services() {
                if main.service_active(&s) {
                    ho.stopped.insert(s.clone());
                }
                let _ = main.stop(&s);
            }
        }
        let mut back_home = Vec::new();
        for s in main.unproxied_services() {
            if active(&s).is_some() {
                if main.service_active(&s) {
                    ho.stopped.insert(s.clone());
                }
                let _ = main.stop(&s);"""),
("""        if let Err(e) = moved {
            ho.stopped.extend(back_home);
            let back = self.give_back(&mut ho);""",
"""        if let Err(e) = moved {
            ho.stopped.extend(back_home);
            drop(handing);
            let back = self.give_back(&mut ho);"""),
("""        let offset = entry.number * self.main.stack.lanes.port_step;
        let services = self
            .main
            .stack
            .services
            .iter()
            .filter_map(|(name, s)| {
                let port = s.port?;
                Some(devstack_node::coordinator::SharedService {""",
"""        let offset = lane_offset(entry.number, self.main.stack.lanes.port_step)?;
        let services = self
            .main
            .stack
            .services
            .iter()
            .filter_map(|(name, s)| {
                let port = s.port?;
                // What a colleague's browser or `devstack open` hands to the
                // OS: a web address, never another protocol's handler.
                if !matches!(s.scheme(), "http" | "https") {
                    crate::diag::warn(
                        "share_scheme_refused",
                        serde_json::json!({ "lane": entry.name, "service": name, "scheme": s.scheme() }),
                    );
                    return None;
                }
                Some(devstack_node::coordinator::SharedService {"""),
("""                ports: ports(&self.main.stack)
                    .into_iter()
                    .filter_map(|p| p.checked_add(entry.number * self.main.stack.lanes.port_step))
                    .map(|p| (p, p))
                    .collect(),
                port_offset: entry.number * self.main.stack.lanes.port_step,""",
"""                ports: ports(&self.main.stack)
                    .into_iter()
                    .filter_map(|p| p.checked_add(offset))
                    .map(|p| (p, p))
                    .collect(),
                port_offset: offset,"""),
("""        let scratch = self.state_dir(&entry.name).join("remote");
        let worktree = devstack_proc::long_path(&entry.path);
        RemoteLane::start(""",
"""        let scratch = self.state_dir(&entry.name).join("remote");
        let worktree = devstack_proc::long_path(&entry.path);
        let offset = lane_offset(entry.number, self.main.stack.lanes.port_step)
            .ok_or_else(|| format!("lane `{}`: its ports are beyond 65535", entry.name))?;
        RemoteLane::start("""),
("""        if let Some(l) = local {
            l.sup.down(true);
            if let Some(l) = self.map.lock().unwrap().remove(name) {
                l.sup.close();
            }
        }""",
"""        if let Some(l) = local {
            l.sup.down(true);
            let removed = self.map.lock().unwrap().remove(name);
            if let Some(l) = removed {
                l.sup.close();
            }
        }"""),
("""        let started = self.activate(&entry.path).and_then(|lane| {
            lane.sup.up().inspect_err(|_| {
                lane.sup.down(true);
                if let Some(l) = self.map.lock().unwrap().remove(name) {
                    l.sup.close();
                }
            })
        });""",
"""        let started = self.activate(&entry.path).and_then(|lane| {
            lane.sup.up().inspect_err(|_| {
                lane.sup.down(true);
                let removed = self.map.lock().unwrap().remove(name);
                if let Some(l) = removed {
                    l.sup.close();
                }
            })
        });"""),
("""        let w = parse_worktrees(text);
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(w[1].branch.as_deref(), Some("feature/a"));
        assert_eq!(w[2].branch, None);
    }""",
"""        let w = parse_worktrees(text);
        assert_eq!(w.len(), 4, "{w:?}");
        assert_eq!(w[1].branch.as_deref(), Some("feature/a"));
        assert_eq!(w[2].branch, None);
        assert!(!w[2].prunable && w[3].prunable, "{w:?}");
    }

    #[test]
    fn lane_offsets_never_wrap() {
        assert_eq!(lane_offset(3, 100), Some(300));
        assert_eq!(lane_offset(199, 1000), None);
        assert_eq!(lane_offset(u16::MAX, 2), None);
    }"""),
])
print("ok")
