//! Lanes (plan §3.5, ADR-0024): the other working trees of the repository
//! (`git worktree`, an agent's checkout) run the stack's services beside the
//! main working tree, inside the same daemon.
//!
//! **The lanes are the repository's worktrees.** `git worktree list` is the
//! source of truth, and git (or whatever tool the developer uses) creates and
//! removes worktrees; devstack never does. A worktree added is a lane at once,
//! not started; one removed is gone, its state with it. The daemon follows
//! `<git common dir>/worktrees` and tells the clients right away (a `lanes`
//! event). `.devstack/lanes.json` only remembers each lane's number, so its
//! ports stay the same.
//!
//! A started lane is a [`Supervisor`] of its own: its source is the worktree,
//! but nothing runs there. Builds run in its snapshot, and so do its dev
//! servers and tools, which the snapshot follows within moments of a change
//! (ADR-0024): the worktree stays free for git to remove at any time. Its
//! state lives in `.devstack/lane-<name>/` (snapshot, logs), and
//! every service port is moved by `port_step` times the lane's number, so a
//! lane's proxies, candidates and dev servers do not meet the main lane's.
//! The infrastructure is the main lane's: a lane reads its published ports,
//! never starts, stops or restarts it, and runs without workers while it does
//! not own the main slot (ADR-0012 interim rule). Per-lane resources on that
//! shared infrastructure (a database per lane) come from `{lane}` /
//! `{lane_suffix}`.
//!
//! The control API of a lane is the same as the main one, under
//! `/v1/lanes/<name>/v1/...`.

use crate::api::{self, AppState};
use crate::supervisor::Supervisor;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A lane as it is remembered across daemon restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneEntry {
    pub name: String,
    /// The lane's working tree (its repository root).
    pub path: PathBuf,
    /// 1, 2, ...: the lane's ports are the stack's plus `number * port_step`.
    pub number: u16,
}

/// A lane as the API shows it.
#[derive(Debug, Clone, Serialize)]
pub struct LaneView {
    pub name: String,
    pub path: PathBuf,
    /// The branch checked out in it (`None`: a detached HEAD).
    pub branch: Option<String>,
    pub number: u16,
    /// What its ports are moved by.
    pub port_offset: u16,
    /// Started (`devstack up` in the worktree): its services may run.
    pub active: bool,
    /// Its instances serve the main slot (`devstack promote --lane`).
    pub owns_main: bool,
    /// Services that run (healthy or starting), of all.
    pub running: usize,
    pub services: usize,
}

/// A started lane.
pub struct Lane {
    pub entry: LaneEntry,
    pub sup: Arc<Supervisor>,
    pub router: axum::Router,
    _watcher: Option<crate::watch::Watcher>,
}

/// A worktree as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
}

#[derive(Debug, Clone)]
struct Known {
    entry: LaneEntry,
    branch: Option<String>,
}

/// What the main working tree gave up for a lane serving the main ports,
/// given back by `promote --lane main`.
#[derive(Default)]
struct Handover {
    /// Its services that ran and were stopped for the lane.
    stopped: BTreeSet<String>,
}

pub struct Lanes {
    root_state: PathBuf,
    main: Arc<Supervisor>,
    proc_opts: devstack_proc::Options,
    rt: tokio::runtime::Handle,
    token: Arc<String>,
    port: u16,
    /// Every worktree of the repository, by lane name (from `git worktree list`).
    known: Mutex<BTreeMap<String, Known>>,
    /// The started lanes.
    map: Mutex<BTreeMap<String, Arc<Lane>>>,
    /// Lanes being started right now, so one is not started twice.
    pending: Mutex<BTreeSet<String>>,
    /// One main-slot hand-over at a time, and what it took from the main tree.
    handover: Mutex<Handover>,
}

/// `Feature/Agent 12` -> `feature-agent-12`; `main` is the main working tree's.
pub fn lane_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect()
}

/// The stack as a lane runs it: every service port moved by `offset`.
pub fn lane_stack(
    stack: &devstack_config::Stack,
    offset: u16,
) -> Result<devstack_config::Stack, String> {
    let mut s = stack.clone();
    for (name, svc) in s.services.iter_mut() {
        if let Some(p) = svc.port {
            svc.port = Some(
                p.checked_add(offset)
                    .ok_or_else(|| format!("`{name}`: port {p} + {offset} is beyond 65535"))?,
            );
        }
    }
    Ok(s)
}

fn ports(stack: &devstack_config::Stack) -> Vec<u16> {
    stack.services.values().filter_map(|s| s.port).collect()
}

/// Two spellings of one directory (`D:/x` from git, `D:\X` from a shell).
fn same_tree(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p)
            .or_else(|_| std::path::absolute(p))
            .unwrap_or_else(|_| p.to_path_buf());
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `git worktree list --porcelain`: every worktree whose directory exists,
/// the main working tree included. `None` when git does not answer: that is
/// no reason to forget a lane.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
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
        parse_worktrees(&text.replace("\r\n", "\n"))
            .into_iter()
            .filter(|w| w.path.is_dir())
            .collect(),
    )
}

/// What changes in `<git common dir>/worktrees` when a worktree is added or
/// removed or checks out another branch: its entries and their `HEAD`s.
fn fingerprint(dir: &Path) -> Vec<(std::ffi::OsString, Option<std::time::SystemTime>)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let head = std::fs::metadata(e.path().join("HEAD"))
                .and_then(|m| m.modified())
                .ok();
            (e.file_name(), head)
        })
        .collect();
    out.sort();
    out
}

impl Lanes {
    fn file(&self) -> PathBuf {
        self.root_state.join("lanes.json")
    }

    pub fn state_dir(&self, name: &str) -> PathBuf {
        self.root_state.join(format!("lane-{name}"))
    }

    /// Written whole to a temporary file and renamed into place, from the
    /// map under its lock: never half a file, never an older snapshot.
    fn save(&self, known: &BTreeMap<String, Known>) {
        let entries: Vec<&LaneEntry> = known.values().map(|k| &k.entry).collect();
        let Ok(text) = serde_json::to_vec_pretty(&entries) else {
            return;
        };
        let tmp = self.root_state.join("lanes.json.tmp");
        let res = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, self.file()));
        if let Err(e) = res {
            crate::diag::warn(
                "lanes_not_saved",
                serde_json::json!({ "error": e.to_string() }),
            );
        }
    }

    /// Set up from the repository's worktrees; none is started (a lane starts
    /// with `devstack up` in its worktree).
    pub fn load(
        root_state: PathBuf,
        main: Arc<Supervisor>,
        proc_opts: devstack_proc::Options,
        rt: tokio::runtime::Handle,
        token: Arc<String>,
        port: u16,
    ) -> Arc<Self> {
        let me = Arc::new(Self {
            root_state,
            main,
            proc_opts,
            rt,
            token,
            port,
            known: Mutex::new(BTreeMap::new()),
            map: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeSet::new()),
            handover: Mutex::new(Handover::default()),
        });
        let remembered: Vec<LaneEntry> = match std::fs::read(me.file()) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                crate::diag::warn(
                    "lanes_unreadable",
                    serde_json::json!({ "error": e.to_string() }),
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        {
            let mut known = me.known.lock().unwrap();
            for e in remembered {
                known.insert(
                    e.name.clone(),
                    Known {
                        entry: e,
                        branch: None,
                    },
                );
            }
        }
        me.sync();
        // The main tree keeps the builds of the started lanes when it prunes.
        let weak = Arc::downgrade(&me);
        me.main.set_lane_pins(Box::new(move |service| {
            weak.upgrade().map(|l| l.pins(service)).unwrap_or_default()
        }));
        me.follow();
        me
    }

    /// Follow `git worktree list`: a new worktree becomes a lane, a removed
    /// one is forgotten with its state. Without an answer from git nothing
    /// changes.
    pub fn sync(&self) {
        let Some(trees) = worktrees(&self.main.repo) else {
            return;
        };
        let trees: Vec<Worktree> = trees
            .into_iter()
            .filter(|w| !same_tree(&w.path, &self.main.repo))
            .collect();
        let gone: Vec<String>;
        let changed;
        {
            let mut known = self.known.lock().unwrap();
            let before = known.len();
            gone = known
                .iter()
                .filter(|(_, k)| !trees.iter().any(|w| same_tree(&w.path, &k.entry.path)))
                .map(|(n, _)| n.clone())
                .collect();
            let mut added = false;
            let mut moved = false;
            for w in &trees {
                if let Some(k) = known
                    .values_mut()
                    .find(|k| same_tree(&k.entry.path, &w.path))
                {
                    if k.branch != w.branch {
                        k.branch = w.branch.clone();
                        moved = true;
                    }
                    continue;
                }
                let base = lane_name(
                    w.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("lane"),
                );
                let base = if base.is_empty() || base == "main" {
                    format!("lane-{base}").trim_end_matches('-').to_string()
                } else {
                    base
                };
                let name = (1..)
                    .map(|i| {
                        if i == 1 {
                            base.clone()
                        } else {
                            format!("{base}-{i}")
                        }
                    })
                    .find(|n| !known.contains_key(n))
                    .expect("a free name");
                let Some(number) = self.free_number(&known, false) else {
                    crate::diag::warn("lane_no_ports", serde_json::json!({ "path": w.path }));
                    continue;
                };
                crate::diag::info(
                    "lane_found",
                    serde_json::json!({ "lane": name, "path": w.path, "number": number }),
                );
                known.insert(
                    name.clone(),
                    Known {
                        entry: LaneEntry {
                            name,
                            path: w.path.clone(),
                            number,
                        },
                        branch: w.branch.clone(),
                    },
                );
                added = true;
            }
            if added || before != known.len() {
                self.save(&known);
            }
            changed = added || moved;
        }
        for name in &gone {
            self.forget(name, "its worktree is gone");
        }
        if changed {
            self.announce();
        }
    }

    /// Tell every client what the lanes are now (a `lanes` event): the main
    /// tree's and those of each started lane, whichever the dashboard shows.
    fn announce(&self) {
        let lanes = self.views();
        let started: Vec<Arc<Lane>> = self.map.lock().unwrap().values().cloned().collect();
        for sup in std::iter::once(&self.main).chain(started.iter().map(|l| &l.sup)) {
            sup.emit(crate::state::EventBody::Lanes {
                lanes: lanes.clone(),
            });
        }
    }

    /// Follow the repository's worktrees while the daemon runs: git keeps one
    /// directory per linked worktree under `<git common dir>/worktrees`, with
    /// its `HEAD`. A change there (a worktree added or removed, a branch
    /// checked out) is a [`Self::sync`] within a second; reading the
    /// directory is all devstack does. A full sync every half minute catches
    /// a worktree deleted without git.
    pub fn follow(self: &Arc<Self>) {
        let Ok(common) = git(&self.main.repo, &["rev-parse", "--git-common-dir"]) else {
            return;
        };
        let common = self.main.repo.join(common.trim());
        let dir = common.join("worktrees");
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("lanes-follow".into())
            .spawn(move || {
                let mut last = fingerprint(&dir);
                let mut since = std::time::Instant::now();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let Some(me) = weak.upgrade() else { return };
                    if me.main.closed() {
                        return;
                    }
                    let now = fingerprint(&dir);
                    if now != last || since.elapsed() >= std::time::Duration::from_secs(30) {
                        last = now;
                        since = std::time::Instant::now();
                        me.sync();
                    }
                }
            });
    }

    /// The first lane number whose port range collides with nothing: the main
    /// lane, another lane and, with `probe`, a port in use on this machine.
    fn free_number(&self, known: &BTreeMap<String, Known>, probe: bool) -> Option<u16> {
        let step = self.main.stack.lanes.port_step;
        let base = ports(&self.main.stack);
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
        })
    }

    /// Stop a lane, if it runs, and drop it with its state.
    fn forget(&self, name: &str, why: &str) {
        if self.main.served_by().as_deref() == Some(name)
            && let Err(e) = self.take_main("main")
        {
            crate::diag::warn(
                "main_slot_back_failed",
                serde_json::json!({ "lane": name, "error": e }),
            );
        }
        if let Some(l) = self.map.lock().unwrap().remove(name) {
            l.sup.close();
        }
        {
            let mut known = self.known.lock().unwrap();
            if known.remove(name).is_some() {
                self.save(&known);
            }
        }
        let _ = std::fs::remove_dir_all(self.state_dir(name));
        crate::diag::info(
            "lane_removed",
            serde_json::json!({ "lane": name, "why": why }),
        );
        self.announce();
    }

    fn open(&self, entry: &LaneEntry) -> Result<Arc<Lane>, String> {
        let repo = devstack_proc::long_path(&entry.path);
        if !repo.is_dir() {
            return Err(format!("{} is gone", repo.display()));
        }
        // The lane's own stack file when its branch has one (it may have changed
        // it), else the main working tree's.
        let own = repo.join(
            self.main
                .stack_file()
                .file_name()
                .unwrap_or("devstack.toml".as_ref()),
        );
        let file = if own.is_file() {
            own
        } else {
            self.main.stack_file().to_path_buf()
        };
        let stack = devstack_config::load(&file).map_err(|e| e.to_string())?;
        // The main stack's step: the free ranges were chosen with it.
        let offset = entry.number * self.main.stack.lanes.port_step;
        let stack = lane_stack(&stack, offset)?;
        let state = self.state_dir(&entry.name);
        std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
        let sup = Supervisor::new_lane(
            stack,
            file,
            repo,
            state,
            self.proc_opts.clone(),
            self.rt.clone(),
            entry.name.clone(),
            self.main.main_owner(),
        )?;
        sup.refresh_infra();
        let usage = crate::usage::Sampler::start(sup.clone());
        let router = api::router(AppState {
            sup: sup.clone(),
            runs: Arc::new(crate::commands::Runs::default()),
            usage,
            token: self.token.clone(),
            port: self.port,
            // `shutdown` inside a lane stops nothing; `down` stops the lane.
            shutdown: Arc::new(tokio::sync::Notify::new()),
            lanes: None,
        });
        let watcher = crate::watch::start(sup.clone());
        Ok(Arc::new(Lane {
            entry: entry.clone(),
            sup,
            router,
            _watcher: watcher,
        }))
    }

    /// A started lane, without asking git (inside a hand-over).
    fn started(&self, name: &str) -> Result<Arc<Lane>, String> {
        self.map
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("lane `{name}` is not started (see `devstack lane list`)"))
    }

    /// A started lane.
    pub fn get(&self, name: &str) -> Result<Arc<Lane>, String> {
        if let Some(l) = self.map.lock().unwrap().get(name) {
            return Ok(l.clone());
        }
        self.sync();
        match self.known.lock().unwrap().get(name) {
            Some(k) => Err(format!(
                "lane `{name}` is not started: `devstack up` in {}",
                k.entry.path.display()
            )),
            None => Err(format!("no lane `{name}` (see `devstack lane list`)")),
        }
    }

    pub fn list(&self) -> Vec<LaneView> {
        self.sync();
        self.views()
    }

    /// The lanes as they are known now, without asking git.
    fn views(&self) -> Vec<LaneView> {
        let map = self.map.lock().unwrap();
        let services = self.main.stack.services.len();
        self.known
            .lock()
            .unwrap()
            .values()
            .map(|k| {
                let lane = map.get(&k.entry.name);
                let (running, services) = match lane {
                    Some(l) => {
                        let v = l.sup.view();
                        let n = v.services.iter().filter(|s| s.status.is_active()).count();
                        (n, v.services.len())
                    }
                    None => (0, services),
                };
                LaneView {
                    name: k.entry.name.clone(),
                    path: k.entry.path.clone(),
                    branch: k.branch.clone(),
                    number: k.entry.number,
                    port_offset: k.entry.number * self.main.stack.lanes.port_step,
                    active: lane.is_some(),
                    owns_main: lane.is_some_and(|l| l.sup.owns_main()),
                    running,
                    services,
                }
            })
            .collect()
    }

    /// Start the lane of the worktree at `path` (`devstack up` in it): its
    /// supervisor is set up; `/v1/lanes/<name>/v1/up` starts its services.
    pub fn activate(&self, path: &Path) -> Result<Arc<Lane>, String> {
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        if same_tree(&path, &self.main.repo) {
            return Err("that is the main working tree; it is the lane `main` already".into());
        }
        self.sync();
        let entry = self
            .known
            .lock()
            .unwrap()
            .values()
            .find(|k| same_tree(&k.entry.path, &path))
            .map(|k| k.entry.clone())
            .ok_or_else(|| {
                format!(
                    "{} is no worktree of {} (`git worktree add` makes one)",
                    path.display(),
                    self.main.repo.display()
                )
            })?;
        let name = entry.name.clone();
        if let Some(l) = self.map.lock().unwrap().get(&name) {
            return Ok(l.clone());
        }
        if !self.pending.lock().unwrap().insert(name.clone()) {
            return Err(format!("lane `{name}` is being started"));
        }
        let result = (|| {
            // Its range taken by another process since it was found: a new one.
            let step = self.main.stack.lanes.port_step;
            let mut entry = entry;
            let busy = ports(&self.main.stack).iter().any(|p| {
                p.checked_add(entry.number * step)
                    .is_none_or(crate::supervisor::port_in_use)
            });
            if busy {
                let mut known = self.known.lock().unwrap();
                let others: BTreeMap<String, Known> = known
                    .iter()
                    .filter(|(n, _)| **n != name)
                    .map(|(n, k)| (n.clone(), k.clone()))
                    .collect();
                entry.number = self
                    .free_number(&others, true)
                    .ok_or("no free port range for another lane")?;
                if let Some(k) = known.get_mut(&name) {
                    k.entry.number = entry.number;
                }
                self.save(&known);
            }
            let lane = self.open(&entry)?;
            self.map.lock().unwrap().insert(name.clone(), lane.clone());
            crate::diag::info(
                "lane_started",
                serde_json::json!({ "lane": name, "path": entry.path, "number": entry.number }),
            );
            Ok(lane)
        })();
        self.pending.lock().unwrap().remove(&name);
        if result.is_ok() {
            self.announce();
        }
        result
    }

    /// `devstack lane stop`: the lane's services stop and it is no longer
    /// started; its snapshot and builds stay for the next start. The worktree
    /// is git's business and stays as it is.
    pub fn stop(&self, name: &str) -> Result<(), String> {
        let lane = self.started(name)?;
        if lane.sup.owns_main() {
            return Err(format!(
                "lane `{name}` serves the main slot: `devstack promote --lane main` first"
            ));
        }
        lane.sup.down(false);
        if let Some(l) = self.map.lock().unwrap().remove(name) {
            l.sup.close();
        }
        crate::diag::info("lane_stopped", serde_json::json!({ "lane": name }));
        self.announce();
        Ok(())
    }

    /// The builds of `service` the started lanes run or may go back to.
    fn pins(&self, service: &str) -> Vec<String> {
        let lanes: Vec<Arc<Lane>> = self.map.lock().unwrap().values().cloned().collect();
        lanes.iter().flat_map(|l| l.sup.pins(service)).collect()
    }

    /// `promote --lane`: the lane `name` serves the main ports (`main`: the
    /// main working tree again). The fixed port of every proxied service
    /// forwards to the lane's port, so candidates in the lane switch on it as
    /// they do on their own port; dev servers the lane runs get a TCP forward
    /// on theirs, the others keep running in the main tree. Workers move with
    /// the main slot (ADR-0008): the old owner's worker services restart
    /// without them, the new owner's with them.
    ///
    /// The ports move first; the owner changes only when all of them did. A
    /// port that cannot move gives everything back to the main tree.
    pub fn take_main(&self, name: &str) -> Result<(), String> {
        let main = &self.main;
        let mut ho = self.handover.lock().unwrap();
        let current = main.served_by();
        let target = (name != "main").then(|| name.to_string());
        if current == target {
            return Ok(());
        }
        let Some(t) = target else {
            return self.give_back(&mut ho);
        };
        let lane = self.started(&t)?;
        let views = lane.sup.view().services;
        let active = |s: &str| {
            views
                .iter()
                .find(|v| v.name == s && v.status.is_active())
                .and_then(|v| v.port.map(|p| (p, v.build_id.clone())))
        };
        let down: Vec<String> = main
            .proxied_services()
            .into_iter()
            .filter(|s| active(s).is_none())
            .collect();
        if !down.is_empty() {
            return Err(format!(
                "lane `{t}` does not run {}: `devstack up` in its working tree first",
                down.join(", ")
            ));
        }
        let prev = current
            .as_deref()
            .and_then(|c| self.map.lock().unwrap().get(c).cloned());

        // 1. Free the fixed ports. Forwards to the previous lane end; the main
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
                }
            } else if ho.stopped.remove(&s) {
                // The previous lane ran it, this one does not: it runs in the
                // main tree again.
                back_home.push(s);
            }
        }

        // 2. Point them at the lane. A failure gives everything back.
        let moved = (|| {
            for s in main.proxied_services() {
                let (port, build) = active(&s).expect("checked above");
                main.forward(&s, Some((&t, port, build)))?;
            }
            for s in main.unproxied_services() {
                if let Some((port, _)) = active(&s) {
                    main.tcp_forward(&s, Some(port))?;
                }
            }
            Ok::<(), String>(())
        })();
        if let Err(e) = moved {
            ho.stopped.extend(back_home);
            let back = self.give_back(&mut ho);
            return Err(match back {
                Ok(()) => format!("{e}; the main working tree serves the main ports again"),
                Err(b) => format!("{e}; giving the main ports back: {b}"),
            });
        }

        // 3. The new owner, and the workers with it.
        main.set_main_owner(Some(t.clone()));
        if let Some(p) = &prev {
            for s in p.sup.running_worker_services() {
                let _ = p.sup.relaunch(&s);
            }
        } else {
            for s in main.running_worker_services() {
                let _ = main.relaunch(&s);
            }
        }
        for s in lane.sup.running_worker_services() {
            let _ = lane.sup.relaunch(&s);
        }
        let mut failed = Vec::new();
        for s in back_home {
            if let Err(e) = main.start(&s) {
                failed.push(format!("{s}: {e}"));
            }
        }
        crate::diag::info("main_slot", serde_json::json!({ "lane": t }));
        self.announce();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "lane `{t}` serves the main ports; in the main tree these did not start again: {}",
                failed.join("; ")
            ))
        }
    }

    /// The main working tree serves the main ports again and runs what it
    /// ran before; the lane's workers come back with them.
    fn give_back(&self, ho: &mut Handover) -> Result<(), String> {
        let main = &self.main;
        let prev = main
            .served_by()
            .and_then(|c| self.map.lock().unwrap().get(&c).cloned());
        for s in main.forwarded_services() {
            let _ = main.tcp_forward(&s, None);
        }
        main.set_main_owner(None);
        for s in main.proxied_services() {
            let _ = main.forward(&s, None);
        }
        if let Some(p) = &prev {
            for s in p.sup.running_worker_services() {
                let _ = p.sup.relaunch(&s);
            }
        }
        // What runs in the main tree all along gets its workers back; what
        // was stopped for the lane starts with them.
        for s in main.running_worker_services() {
            let _ = main.relaunch(&s);
        }
        let mut failed = Vec::new();
        for s in main.service_order() {
            if ho.stopped.remove(&s)
                && let Err(e) = main.start(&s)
            {
                failed.push(format!("{s}: {e}"));
            }
        }
        ho.stopped.clear();
        crate::diag::info("main_slot", serde_json::json!({ "lane": "main" }));
        self.announce();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the main working tree serves the main ports again, but these did not start: {}",
                failed.join("; ")
            ))
        }
    }

    /// Stop every lane's services (the daemon is stopping).
    pub fn down_all(&self) {
        let lanes: Vec<Arc<Lane>> = self.map.lock().unwrap().values().cloned().collect();
        for l in lanes {
            l.sup.down(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ports() {
        assert_eq!(lane_name("Feature/Agent 12"), "feature-agent-12");
        assert_eq!(lane_name("--x--"), "x");
        let stack = devstack_config::from_str(
            "version = 1\n[stack]\nname = \"t\"\n[services.api]\nrun = \"x\"\nport = 5000\n[services.job]\nkind = \"job\"\nrun = \"y\"\n",
            Path::new("t.toml"),
        )
        .unwrap();
        assert_eq!(stack.lanes.port_step, 100);
        let l = lane_stack(&stack, 200).unwrap();
        assert_eq!(l.services["api"].port, Some(5200));
        assert_eq!(l.services["job"].port, None);
        assert!(lane_stack(&stack, 65000).is_err());
    }

    #[test]
    fn worktree_list_is_parsed() {
        let text = "worktree /r/main\nHEAD abc\nbranch refs/heads/main\n\n\
                    worktree /r/wt-a\nHEAD def\nbranch refs/heads/feature/a\n\n\
                    worktree /r/wt-b\nHEAD 123\ndetached\n\n\
                    worktree /r/gone\nHEAD 456\nbranch refs/heads/x\nprunable gitdir file points to non-existent location\n";
        let w = parse_worktrees(text);
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(w[1].branch.as_deref(), Some("feature/a"));
        assert_eq!(w[2].branch, None);
    }

    #[test]
    fn spellings_of_one_directory_are_the_same_tree() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let other = PathBuf::from(p.to_string_lossy().replace('\\', "/") + "/");
        assert!(same_tree(p, &other));
        assert!(!same_tree(p, &p.join("x")));
    }
}
