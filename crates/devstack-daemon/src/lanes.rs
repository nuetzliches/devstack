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
//!
//! A lane may run on another machine of the pool instead (`devstack up --on
//! <node>`, ADR-0025): then it is a [`RemoteLane`], and its API is the
//! node's lane daemon, passed through under the same path.

use crate::api::{self, AppState};
use crate::remote::RemoteLane;
use crate::state::{Notice, NoticeLevel};
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
    /// The Compose containers of its own it starts that run, of all: a stack
    /// of containers only shows by them that it runs.
    pub containers_running: usize,
    pub containers: usize,
    /// The node it runs on; `None`: this machine (ADR-0025 point 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The pool of that node (ADR-0034).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Whom it is shared with in the pool (ADR-0035): member ids or tags,
    /// `*` for every member; empty: nobody.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<String>,
    /// The profile it runs with (ADR-0041), while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// What went wrong with a lane on a node lately (a sync, a renewal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_error: Option<String>,
    /// The node or the coordinator runs another devstack than this machine,
    /// or does not say which (ADR-0031, amended 2026-10-19).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_warning: Option<String>,
    /// Of a lane on a node, its containers' published ports this machine
    /// does not forward (`5353/udp`): another protocol than TCP and UDP
    /// (#192), or UDP the node does not relay (ADR-0056).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_forwarded: Vec<String>,
    /// Why they are not, when there are such ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_forwarded_reason: Option<String>,
    /// A start on a node or a move under way, or one that failed lately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moving: Option<LaneMove>,
    /// Its worktree is gone, and the lane is being removed with it (its
    /// services stop, its data goes): nothing can be done with it any more.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub removing: bool,
    /// Its worktree has no stack file of its own: it runs with the main
    /// working tree's (ADR-0055).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub main_stack_file: bool,
    /// Its notices (ADR-0050): its own tree's while it runs here, and what
    /// the main tree's daemon knows of it (its worktree missing).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
}

/// Of a lane, what the lists show that changes on its own: name, services
/// and containers running and all, its node's error, whether it moves, its
/// notices, its ports not forwarded, whether it has a stack file.
type Shown = (
    String,
    [usize; 4],
    Option<String>,
    bool,
    Vec<Notice>,
    Vec<String>,
    bool,
);

/// The notice of a lane whose worktree git lists but is not there, missing
/// since `since` (unix ms): the lane goes after [`MISSING_GRACE`].
/// A service's state in a few words, for a refusal that names it.
fn status_text(s: &crate::state::Status) -> String {
    use crate::state::Status;
    match s {
        Status::Stopped => "stopped".into(),
        Status::Failed { reason } => {
            format!(
                "failed: {}",
                reason.lines().next().unwrap_or_default().trim()
            )
        }
        Status::Exited { exit_code } => format!(
            "exited with code {}",
            crate::state::exit_code_text(*exit_code)
        ),
        Status::Completed { exit_code } => format!("completed with code {exit_code}"),
        Status::Unhealthy { .. } => "unhealthy".into(),
        _ => "not ready".into(),
    }
}

fn missing_notice(path: &Path, since: u128) -> Notice {
    Notice {
        id: "worktree_missing".into(),
        level: NoticeLevel::Error,
        text: format!(
            "Its worktree {} is not there, though git still lists it. Unless it is back within {} minutes, the lane goes, with its containers and volumes.",
            path.display(),
            MISSING_GRACE.as_secs() / 60
        ),
        since,
    }
}

/// A lane being started on a node (`up --on`) or moved (`lane move`), as the
/// clients show it: step `step` of `steps`, what it does, or why it failed.
#[derive(Debug, Clone, Serialize)]
pub struct LaneMove {
    /// `start` or `move`.
    pub kind: &'static str,
    /// The node, or `local`.
    pub to: String,
    pub step: u8,
    pub steps: u8,
    pub what: String,
    /// Unix ms when it began.
    pub since: u128,
    /// Why it stopped; shown for a while, then gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Unix ms when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<u128>,
    /// The node or the coordinator runs another devstack (ADR-0031 point
    /// 8), known from the lease on: shown as the lane's `node_warning`
    /// until the lane runs there and says it itself.
    #[serde(skip)]
    pub warning: Option<String>,
    /// The phases over so far, each with its time as seen here: what the
    /// start view lists while the lane has no pages yet.
    pub phases: Vec<MovePhase>,
    /// Unix ms the current phase began.
    pub phase_since: u128,
    /// The current phase, as [`phase_of`] names it.
    #[serde(skip)]
    phase: String,
}

/// A phase of a start or move that is over.
#[derive(Debug, Clone, Serialize)]
pub struct MovePhase {
    pub phase: String,
    pub ms: u64,
}

/// The phase a step's text belongs to: a sync's progress (`sync: 3 of 40
/// changed files sent`) is one phase, `sync`; a node's phase without the
/// node's name (`on n1: infrastructure: app: starting containers`).
fn phase_of(what: &str) -> String {
    if what.starts_with("sync:") {
        return "sync".into();
    }
    match what.strip_prefix("on ").and_then(|r| r.split_once(": ")) {
        Some((_, p)) => p.to_string(),
        None => what.to_string(),
    }
}

/// What [`Lanes::activate_remote`] did with a lane: named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteStart {
    /// It runs on that node already.
    Running(String),
    /// Its start there goes on in the background (its `moving`).
    Starting(String),
}

/// How long a failed start or move stays shown.
const MOVE_ERROR_FOR_MS: u128 = 10 * 60 * 1000;

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
}

/// Whom each lane is shared with, from `.devstack/lane-shares.json`. The
/// separate viewers devstack 0.11 kept (`lane-views.json`, ADR-0040 before
/// its amendment of 2026-10-20) could use the lane as well: they are added
/// to its sharing once, and the file goes. `default`: `[lanes] share`, for
/// a lane its owner never shared.
fn load_shares(state: &Path, default: &[String]) -> BTreeMap<String, Vec<String>> {
    let read = |f: &str| -> Option<BTreeMap<String, Vec<String>>> {
        serde_json::from_slice(&std::fs::read(state.join(f)).ok()?).ok()
    };
    let mut shares = read("lane-shares.json").unwrap_or_default();
    let views_file = state.join("lane-views.json");
    if !views_file.exists() {
        return shares;
    }
    for (lane, view) in read("lane-views.json").unwrap_or_default() {
        if view.is_empty() {
            continue;
        }
        let with = shares.entry(lane).or_insert_with(|| default.to_vec());
        for v in view {
            if !with.contains(&v) {
                with.push(v);
            }
        }
    }
    let saved = std::fs::write(
        state.join("lane-shares.json"),
        serde_json::to_vec_pretty(&shares).expect("shares serialise"),
    );
    if saved.is_ok() {
        let _ = std::fs::remove_file(&views_file);
    }
    shares
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
    /// Its services that ran and were stopped for the lane, with the build
    /// each ran: that one starts again, not a candidate held meanwhile
    /// (ADR-0013: a held build reaches the main slot through `promote`).
    stopped: BTreeMap<String, Option<String>>,
}

/// While it lives, the main ports are moving to a lane: the main tree's
/// starts keep off them ([`Supervisor::set_handing_over`]).
struct Handing<'a>(&'a Supervisor);

impl Drop for Handing<'_> {
    fn drop(&mut self) {
        self.0.set_handing_over(false);
    }
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
    /// The started lanes that run on a node of the pool.
    remote: Mutex<BTreeMap<String, Arc<RemoteLane>>>,
    /// Starts on a node and moves under way, or failed lately.
    moves: Mutex<BTreeMap<String, LaneMove>>,
    /// Lanes being started, moved or forgotten right now, so one is not
    /// started twice.
    pending: Mutex<BTreeSet<String>>,
    /// Of `pending`, the lanes being forgotten: what a second forget says.
    forgetting: Mutex<BTreeSet<String>>,
    /// Whom each lane is shared with, where its owner said so (ADR-0035);
    /// else `[lanes] share`. They may view it too (ADR-0040, amended
    /// 2026-10-20). Kept in `.devstack/lane-shares.json`.
    shares: Mutex<BTreeMap<String, Vec<String>>>,
    /// The profile each started lane runs with (ADR-0041), while it runs.
    profiles: Mutex<BTreeMap<String, String>>,
    /// One main-slot hand-over at a time, and what it took from the main tree.
    handover: Mutex<Handover>,
    /// One [`Lanes::sync`] at a time: asking git and applying the answer go
    /// together, or an answer from before a worktree was added could be
    /// applied after a newer one and drop it as gone.
    syncing: Mutex<()>,
    /// Lanes whose worktree git still lists but whose directory is not
    /// there, since when ([`MISSING_GRACE`]).
    missing: Mutex<BTreeMap<String, std::time::Instant>>,
    /// The daemon is stopping ([`Lanes::down_all`]): the main tree takes no
    /// main slot back and starts nothing for it.
    closing: std::sync::atomic::AtomicBool,
    /// It stops for a restart (ADR-0052): [`Lanes::down_all`] leaves the
    /// lanes for the next daemon instead of releasing those on nodes.
    restart: std::sync::atomic::AtomicBool,
    /// What a restart carries over, noted when the daemon begins to stop and
    /// written when it is done ([`Lanes::leave_all`]).
    carry: Mutex<Option<Carry>>,
}

/// What a daemon that restarts leaves for the next one, in
/// `.devstack/restart.json` (ADR-0052).
#[derive(Debug, Serialize, Deserialize)]
struct Carry {
    /// Unix ms it was written.
    written: u128,
    /// `[stack] name`: it was written for this stack.
    stack: String,
    lanes: Vec<CarriedLane>,
    /// The lane that served the main ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CarriedLane {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    /// On a node: its lease and ports; absent: it ran on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<crate::remote::Carried>,
}

const CARRY_FILE: &str = "restart.json";
/// How long a restart's word holds for the next daemon: less than a lease
/// the old daemon renewed (every 30 s) lives without a renewal (150 s).
const CARRY_FOR: std::time::Duration = std::time::Duration::from_secs(120);

/// What `prune` or `reset` found, and did.
#[derive(Debug, Clone, Serialize)]
pub struct Pruned {
    /// `this machine`, or the node (`<pool>:<node>` beyond the default pool).
    pub place: String,
    pub lane: String,
    /// What its kept directory takes on a node (volumes not counted).
    pub bytes: Option<u64>,
    pub why: String,
    pub removed: bool,
    pub error: Option<String>,
}

/// Every Compose project on this machine's engine, running or not, with
/// its Compose files (as `docker compose ls` lists them, comma-separated).
fn compose_projects() -> Vec<(String, String)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Project {
        name: String,
        #[serde(default)]
        config_files: String,
    }
    let out = std::process::Command::new("docker")
        .args(["compose", "ls", "-a", "--format", "json"])
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Vec<Project>>(&o.stdout).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.name, p.config_files))
        .collect()
}

/// `path` among Compose's comma-separated `files`, spelled either way.
fn among(files: &str, path: &Path) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    // Under it, not beside it: `lane-fix` is not `lane-fixed`.
    let want = format!("{}/", norm(&path.to_string_lossy()).trim_end_matches('/'));
    files.split(',').any(|f| norm(f.trim()).starts_with(&want))
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
    for g in s.compose.values_mut() {
        for url in g.urls.values_mut() {
            *url = lane_url(url, offset);
        }
    }
    Ok(s)
}

/// A link of `[compose.<group>] urls` as a lane reaches it: a port on this
/// machine (`localhost`, `127.0.0.1`, `[::1]`) moves by the lane's offset, as
/// every port of the lane does; another host, or a URL without a port,
/// stays as it is written (#101).
fn lane_url(url: &str, offset: u16) -> String {
    let moved = || {
        let (scheme, rest) = url.split_once("://")?;
        let (authority, path) = rest
            .find(['/', '?', '#'])
            .map_or((rest, ""), |i| rest.split_at(i));
        let (host, port) = authority.rsplit_once(':')?;
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return None;
        }
        let port = port.parse::<u16>().ok()?.checked_add(offset)?;
        Some(format!("{scheme}://{host}:{port}{path}"))
    };
    if offset == 0 {
        return url.to_string();
    }
    moved().unwrap_or_else(|| url.to_string())
}

/// The services a lane shares, with its ports moved by `offset`: a lane on
/// a node listens on its lane ports there as it does here (see `ports`
/// where it starts), the same port on both ends. Web addresses only: what
/// a colleague's browser or `devstack open` hands to the OS is never
/// another protocol's handler.
///
/// Its Compose containers too, those the dashboard links as a web page
/// (`[compose.<group>] urls`, or `http://localhost:<port>` per published
/// port without an entry; `""` keeps one out): `published` holds the main
/// tree's host ports, `(group, service, container port) -> host:port`, and
/// the lane publishes them moved by its offset. Not the group's `shared`
/// containers: those are the main tree's, not the lane's.
fn shared_services(
    stack: &devstack_config::Stack,
    offset: u16,
    lane: &str,
    published: &BTreeMap<(String, String, u16), String>,
) -> Vec<devstack_node::coordinator::SharedService> {
    let mut out: Vec<devstack_node::coordinator::SharedService> = stack
        .services
        .iter()
        .filter_map(|(name, s)| {
            let port = s.port?;
            if !matches!(s.scheme(), "http" | "https") {
                crate::diag::warn(
                    "share_scheme_refused",
                    serde_json::json!({ "lane": lane, "service": name, "scheme": s.scheme() }),
                );
                return None;
            }
            let port = port.checked_add(offset)?;
            Some(devstack_node::coordinator::SharedService {
                name: name.clone(),
                port,
                scheme: s.scheme().into(),
                here: port,
            })
        })
        .collect();
    for ((group, service, _), addr) in published {
        let Some(g) = stack.compose.get(group) else {
            continue;
        };
        if (!g.services.is_empty() && !g.services.contains(service)) || g.shared.contains(service) {
            continue;
        }
        let Some(host) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) else {
            continue;
        };
        let (scheme, port) = match g.urls.get(service) {
            Some(u) if u.is_empty() => continue,
            // The entry's port when it names one on this machine; else the
            // published one (a link elsewhere is not the lane's).
            Some(u) => match url_port(u) {
                Some((scheme, p)) if p == host => (scheme, p),
                Some(_) => continue,
                None => continue,
            },
            None => ("http", host),
        };
        let Some(port) = port.checked_add(offset) else {
            continue;
        };
        if out.iter().any(|s| s.port == port) {
            continue;
        }
        let name = if out.iter().any(|s| &s.name == service) {
            format!("{group}.{service}")
        } else {
            service.clone()
        };
        out.push(devstack_node::coordinator::SharedService {
            name,
            port,
            scheme: scheme.into(),
            here: port,
        });
    }
    out
}

/// `http`/`https` and the port of a link on this machine
/// (`http://localhost:5100/ui`); `None` for anything else.
fn url_port(url: &str) -> Option<(&'static str, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" => "http",
        "https" => "https",
        _ => return None,
    };
    let authority = rest.find(['/', '?', '#']).map_or(rest, |i| &rest[..i]);
    let (host, port) = authority.rsplit_once(':')?;
    if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    Some((scheme, port.parse().ok()?))
}

fn ports(stack: &devstack_config::Stack) -> Vec<u16> {
    stack.services.values().filter_map(|s| s.port).collect()
}

/// Two spellings of one directory (`D:/x` from git, `D:\X` from a shell).
pub fn same_tree(a: &Path, b: &Path) -> bool {
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

/// `git worktree list --porcelain`: every worktree but a bare one, the main
/// working tree included.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
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
        parse_worktrees(&text.replace("\r\n", "\n"))
            .into_iter()
            .partition(|w| !w.prunable && w.path.is_dir());
    Some((here, missing.into_iter().map(|w| w.path).collect()))
}

/// The worktrees of `repo` whose directory exists, the main working tree
/// first, each with its branch (`None`: a detached `HEAD`): what the hub
/// marks the branches with (ADR-0047). `None` when git does not answer.
pub fn worktree_branches(repo: &Path) -> Option<Vec<(PathBuf, Option<String>)>> {
    let (here, _) = worktrees(repo)?;
    Some(here.into_iter().map(|w| (w.path, w.branch)).collect())
}

/// The worktree of `repo` (the main working tree included) that `path`
/// names, as git lists it; `repo` itself also without git. `None` when
/// `path` is none of them. What the hub opens (ADR-0046) is checked against
/// it.
pub fn worktree_of(repo: &Path, path: &Path) -> Option<PathBuf> {
    if same_tree(repo, path) {
        return Some(repo.to_path_buf());
    }
    let (here, _) = worktrees(repo)?;
    here.into_iter()
        .map(|w| w.path)
        .find(|w| same_tree(w, path))
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

/// Remove a forgotten lane's state directory (its snapshot, its logs). On
/// Windows a file still open (a sampler walking it, a virus scanner, a
/// process of the lane that is just going) fails the delete: it is said, and
/// tried again for a while; what still stays is swept at the next daemon
/// start ([`sweep_state_dirs`]). A lane of the same name found again
/// meanwhile (`lanes` lists it) keeps its directory.
fn remove_state_dir(lane: &str, dir: PathBuf, lanes: PathBuf) {
    let first = match std::fs::remove_dir_all(&dir) {
        Ok(()) => return,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => e,
    };
    crate::diag::warn(
        "lane_state_not_removed",
        serde_json::json!({ "lane": lane, "error": first.to_string(), "retry": true }),
    );
    let lane = lane.to_string();
    let _ = std::thread::Builder::new()
        .name(format!("lane-remove-{lane}"))
        .spawn(move || {
            let mut last = first;
            // Longer than a disk sample of the lane takes to notice it is closed.
            for _ in 0..24 {
                std::thread::sleep(std::time::Duration::from_secs(5));
                // Unreadable counts as listed: never remove what may be in use.
                let listed = std::fs::read(&lanes)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Vec<LaneEntry>>(&b).ok())
                    .is_none_or(|l| l.iter().any(|e| e.name == lane));
                if listed {
                    return;
                }
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => {
                        crate::diag::info(
                            "lane_state_removed",
                            serde_json::json!({ "lane": lane }),
                        );
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                    Err(e) => last = e,
                }
            }
            crate::diag::warn(
                "lane_state_not_removed",
                serde_json::json!({ "lane": lane, "error": last.to_string(), "retry": false,
                    "why": "the next daemon start removes it" }),
            );
        });
}

/// State directories (`lane-<name>`) of lanes no longer known: a removal
/// that failed before (a file held open on Windows). Only with the list of
/// lanes read (`known`): without it every lane would look forgotten.
fn sweep_state_dirs(root: &Path, known: &BTreeSet<String>) {
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let file_name = e.file_name();
        let Some(lane) = file_name.to_str().and_then(|n| n.strip_prefix("lane-")) else {
            continue;
        };
        if known.contains(lane) || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let r = std::fs::remove_dir_all(e.path());
        crate::diag::info(
            "lane_state_swept",
            serde_json::json!({ "lane": lane, "error": r.err().map(|e| e.to_string()) }),
        );
    }
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
        let shares = load_shares(&root_state, &main.stack.lanes.share);
        let me = Arc::new(Self {
            root_state,
            main,
            proc_opts,
            rt,
            token,
            port,
            known: Mutex::new(BTreeMap::new()),
            map: Mutex::new(BTreeMap::new()),
            remote: Mutex::new(BTreeMap::new()),
            moves: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeSet::new()),
            forgetting: Mutex::new(BTreeSet::new()),
            shares: Mutex::new(shares),
            profiles: Mutex::new(BTreeMap::new()),
            handover: Mutex::new(Handover::default()),
            syncing: Mutex::new(()),
            missing: Mutex::new(BTreeMap::new()),
            closing: std::sync::atomic::AtomicBool::new(false),
            restart: std::sync::atomic::AtomicBool::new(false),
            carry: Mutex::new(None),
        });
        let remembered: Option<Vec<LaneEntry>> = match std::fs::read(me.file()) {
            Ok(b) => serde_json::from_slice(&b)
                .inspect_err(|e| {
                    crate::diag::warn(
                        "lanes_unreadable",
                        serde_json::json!({ "error": e.to_string() }),
                    );
                })
                .ok(),
            Err(_) => None,
        };
        let read = remembered.is_some();
        let remembered = remembered.unwrap_or_default();
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
        if read {
            let known: BTreeSet<String> = me.known.lock().unwrap().keys().cloned().collect();
            sweep_state_dirs(&me.root_state, &known);
        }
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
        let (trees, missing) = worktrees(&self.main.repo)?;
        if let Some(main) = trees.iter().find(|w| same_tree(&w.path, &self.main.repo)) {
            self.main.set_branch(main.branch.clone());
        }
        for l in self.map.lock().unwrap().values() {
            if let Some(w) = trees.iter().find(|w| same_tree(&w.path, &l.entry.path)) {
                l.sup.set_branch(w.branch.clone());
            }
        }
        let trees: Vec<Worktree> = trees
            .into_iter()
            .filter(|w| !same_tree(&w.path, &self.main.repo))
            .collect();
        let mut gone: Vec<String>;
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
                        serde_json::json!({ "lane": n, "worktree": known[n.as_str()].entry.path,
                            "forgotten_after_s": MISSING_GRACE.as_secs() }),
                    );
                    std::time::Instant::now()
                });
                t.elapsed() >= MISSING_GRACE
            });
            let mut added = false;
            let mut moved = false;
            let mut no_ports = Vec::new();
            for w in &trees {
                // Its path stays on this machine (ADR-0037 point 4).
                crate::diag::add_root(&w.path, "<worktree>");
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
                    no_ports.push(w.path.clone());
                    continue;
                };
                crate::diag::info(
                    "lane_found",
                    serde_json::json!({ "lane": name, "worktree": w.path, "number": number }),
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
            self.note_no_ports(&no_ports);
        }
        // A lane that runs says in its own view that its worktree is missing.
        let missing = self.missing.lock().unwrap().clone();
        for (name, l) in self.map.lock().unwrap().iter() {
            match missing.get(name) {
                Some(_) => {
                    let n = missing_notice(&l.entry.path, crate::state::now_ms());
                    l.sup.set_notice(&n.id, n.level, n.text);
                }
                None => l.sup.clear_notice("worktree_missing"),
            }
        }
        Some((gone, changed))
    }

    /// The worktrees that got no lane for want of a port range, as notices
    /// of the main tree (ADR-0050), logged once each.
    fn note_no_ports(&self, worktrees: &[PathBuf]) {
        let id = |p: &PathBuf| format!("no_ports.{}", p.display());
        let now: BTreeSet<String> = worktrees.iter().map(id).collect();
        self.main
            .clear_notices(|n| n.starts_with("no_ports.") && !now.contains(n));
        for w in worktrees {
            let text = format!(
                "The worktree {} gets no lane: no lane number up to 199 moves the stack's ports to ones free of the other lanes and below 65536 (`[lanes] port_step` is {}). Remove a worktree no longer needed, or lower `port_step`.",
                w.display(),
                self.main.stack.lanes.port_step
            );
            if self.main.set_notice(&id(w), NoticeLevel::Warn, text) {
                crate::diag::warn("lane_no_ports", serde_json::json!({ "worktree": w }));
            }
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
                let mut shown = Vec::new();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let Some(me) = weak.upgrade() else { return };
                    if me.main.closed() {
                        return;
                    }
                    me.main_slot_check();
                    // What changes without a call here: a lane's services
                    // starting and stopping, a lane on a node as read every
                    // few seconds, a failed start that expired.
                    let now = me.shown();
                    if now != shown {
                        shown = now;
                        me.announce();
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

    /// Of each lane, what the lists show that changes on its own.
    fn shown(&self) -> Vec<Shown> {
        self.views()
            .into_iter()
            .map(|v| {
                (
                    v.name,
                    [v.running, v.services, v.containers_running, v.containers],
                    v.node_error,
                    v.moving.is_some(),
                    v.notices,
                    v.not_forwarded,
                    v.main_stack_file,
                )
            })
            .collect()
    }

    /// A lane on a node that serves the main ports and is gone (its lease
    /// over, its node not answering): the main tree takes them back
    /// (ADR-0025 point 4).
    fn main_slot_check(&self) {
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(owner) = self.main.served_by() else {
            return;
        };
        if self.map.lock().unwrap().contains_key(&owner) {
            return;
        }
        let gone = self.remote(&owner).is_none_or(|r| r.state().ended);
        if !gone {
            return;
        }
        let Ok(mut ho) = self.handover.try_lock() else {
            return;
        };
        if self.main.served_by().as_deref() != Some(owner.as_str()) {
            return;
        }
        crate::diag::warn(
            "main_slot_lost",
            serde_json::json!({ "lane": owner, "reason": "the lane on its node is gone" }),
        );
        let back = self.give_back(&mut ho);
        if let Err(e) = &back {
            crate::diag::warn(
                "main_slot_back_failed",
                serde_json::json!({ "lane": owner, "error": e }),
            );
        }
        self.main.tell(
            if back.is_ok() {
                NoticeLevel::Warn
            } else {
                NoticeLevel::Error
            },
            format!(
                "Lane {owner} served the main ports and is gone (its lease ended, or its node no longer answers): the main tree serves them again.{}",
                back.err()
                    .map(|e| format!(" These did not start again: {e}"))
                    .unwrap_or_default()
            ),
        );
    }

    /// The first lane number whose port range collides with nothing: the main
    /// lane, another lane and, with `probe`, a port in use on this machine.
    fn free_number(&self, known: &BTreeMap<String, Known>, probe: bool) -> Option<u16> {
        let step = self.main.stack.lanes.port_step;
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
        })
    }

    /// Stop a lane, if it runs, and drop it with its state. Not while it is
    /// being started (its supervisor is being set up from that state): the
    /// next sync forgets it, once the start is over.
    fn forget(&self, name: &str, why: &str) {
        // Held for the whole of it: a start that comes in meanwhile is
        // refused ("is being started"), and finds the lane gone after.
        if !self.pending.lock().unwrap().insert(name.to_string()) {
            // `pending` is a start's, a move's or another forget's: say whose.
            let busy = if self.forgetting.lock().unwrap().contains(name) {
                "it is being removed already"
            } else {
                "it is being started or moved"
            };
            crate::diag::info(
                "lane_forget_deferred",
                serde_json::json!({ "lane": name, "why": busy }),
            );
            return;
        }
        self.forgetting.lock().unwrap().insert(name.to_string());
        // Stopping it and removing its data takes a while (its containers
        // and volumes): the clients see it go from the start.
        self.announce();
        self.forget_claimed(name, why);
        self.forgetting.lock().unwrap().remove(name);
        self.pending.lock().unwrap().remove(name);
        self.announce();
    }

    fn forget_claimed(&self, name: &str, why: &str) {
        // A lane on a node syncs its worktree no more: what goes of it now
        // is not the lane's to lose there while the main slot comes back.
        if let Some(r) = self.remote(name) {
            r.hold_sync();
        }
        if self.main.served_by().as_deref() == Some(name)
            && let Err(e) = self.take_main("main")
        {
            crate::diag::warn(
                "main_slot_back_failed",
                serde_json::json!({ "lane": name, "error": e }),
            );
            self.main.tell(
                NoticeLevel::Error,
                format!(
                    "Lane {name} served the main ports and was removed ({why}): the main tree took them back, but not all of its services started again: {e}"
                ),
            );
        }
        // Not under the map's lock: stopping its services takes a while, and
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
        self.profiles.lock().unwrap().remove(name);
        // Logged there; a lane that is forgotten has nobody to tell.
        let _ = self.remove_infra(name);
        // Its kept data on the nodes it ran on goes too (ADR-0038 point 4);
        // a node that does not answer keeps it until its retention ends.
        let ran = crate::remote::ran_on(&self.state_dir(name));
        if !ran.is_empty() {
            let (rt, repo, lane) = (
                self.rt.clone(),
                self.main.stack.stack.name.clone(),
                name.to_string(),
            );
            std::thread::spawn(move || {
                for on in ran {
                    let r = crate::remote::purge_slot(&rt, &on, &repo, &lane);
                    crate::diag::info(
                        "lane_data_removed",
                        serde_json::json!({ "lane": lane, "node": on.node.name,
                            "error": r.err() }),
                    );
                }
            });
        }
        {
            let mut known = self.known.lock().unwrap();
            if known.remove(name).is_some() {
                self.save(&known);
            }
        }
        remove_state_dir(name, self.state_dir(name), self.file());
        crate::diag::info(
            "lane_removed",
            serde_json::json!({ "lane": name, "why": why }),
        );
    }

    /// Throw the data of lane `name` away, here and on the nodes it ran on
    /// (ADR-0038 point 5): its containers and volumes; its next start is a
    /// fresh one, seeded again. Not while it runs.
    pub fn reset(&self, name: &str) -> Result<Vec<Pruned>, String> {
        if !self.known.lock().unwrap().contains_key(name) {
            return Err(format!("no lane `{name}` (see `devstack lane list`)"));
        }
        if self.map.lock().unwrap().contains_key(name) || self.remote(name).is_some() {
            return Err(format!(
                "lane `{name}` runs: `devstack lane stop {name}` first"
            ));
        }
        let here = self.remove_infra(name);
        let mut out = vec![Pruned {
            place: "this machine".into(),
            lane: name.to_string(),
            bytes: None,
            why: "reset".into(),
            removed: here.is_ok(),
            error: here.err(),
        }];
        let repo = self.main.stack.stack.name.clone();
        for on in crate::remote::ran_on(&self.state_dir(name)) {
            let r = crate::remote::purge_slot(&self.rt, &on, &repo, name);
            out.push(Pruned {
                place: on.node.name.clone(),
                lane: name.to_string(),
                bytes: None,
                why: "reset".into(),
                removed: r.is_ok(),
                error: r.err(),
            });
        }
        crate::diag::info("lane_reset", serde_json::json!({ "lane": name }));
        Ok(out)
    }

    /// What this repository's lanes leave behind that no lane needs any
    /// more (ADR-0038 point 5): on this machine, the Compose projects of
    /// lanes that are gone; on the nodes of every pool, this member's kept
    /// lanes of this repository that are gone, or (with `older_than`) not
    /// used for that long. With `dry_run` only said.
    pub fn prune(&self, older_than: Option<std::time::Duration>, dry_run: bool) -> Vec<Pruned> {
        let known: BTreeSet<String> = self.known.lock().unwrap().keys().cloned().collect();
        let mut out = Vec::new();
        // This machine: `<project>-<lane>` of a lane no worktree has.
        let projects = compose_projects();
        for (g, cfg) in &self.main.stack.compose {
            let prefix = format!("{}-", cfg.project);
            for (p, files) in &projects {
                let Some(lane) = p.strip_prefix(&prefix) else {
                    continue;
                };
                if lane.is_empty() || known.contains(lane) {
                    continue;
                }
                // Only a lane of this repository: its overlay is in this
                // repository's lane state. Another repository's project of
                // the same prefix is not touched.
                if !among(files, &self.state_dir(lane)) {
                    continue;
                }
                // Another group's project that starts the same (`app` and
                // `app-tools`) is not a lane's.
                if self.main.stack.compose.values().any(|c| c.project == *p) {
                    continue;
                }
                let mut removed = false;
                let mut error = None;
                if !dry_run {
                    let group = devstack_compose::Group {
                        name: g.clone(),
                        project: p.clone(),
                        files: Vec::new(),
                        env_files: Vec::new(),
                        networks: cfg.networks.iter().map(|n| format!("{n}-{lane}")).collect(),
                        services: Vec::new(),
                        dir: self.main.repo.clone(),
                        // No files: nothing for `DEVSTACK_PORT_*` or `DEVSTACK_LANE*` to fill in.
                        env: Default::default(),
                    };
                    match devstack_compose::Docker::default().remove(&group) {
                        Ok(()) => removed = true,
                        Err(e) => error = Some(e.to_string()),
                    }
                }
                out.push(Pruned {
                    place: "this machine".into(),
                    lane: lane.to_string(),
                    bytes: None,
                    why: format!("its worktree is gone (compose project `{p}`)"),
                    removed,
                    error,
                });
            }
        }
        // The nodes of every pool this machine is in.
        let repo = self.main.stack.stack.name.clone();
        let now = crate::state::now_ms() as u64 / 1000;
        for (pool, _) in devstack_node::member::pools(&crate::remote::state_dir()) {
            let local = crate::remote::local_member(&pool);
            let Ok(nodes) = local.nodes() else { continue };
            for n in nodes {
                let Some(api) = n.report.as_ref().and_then(|r| r.api.clone()) else {
                    continue;
                };
                if !n.online {
                    continue;
                }
                let node = devstack_node::coordinator::NodeRef {
                    id: n.id.clone(),
                    name: n.name.clone(),
                    api,
                    devstack: None,
                };
                let Ok(slots) = crate::remote::slots_on(&self.rt, &pool, node.clone()) else {
                    continue;
                };
                for s in slots.into_iter().filter(|s| s.repo == repo && !s.active) {
                    let why = if !known.contains(&s.lane) {
                        "its worktree is gone".to_string()
                    } else if older_than.is_some_and(|o| now.saturating_sub(s.used) >= o.as_secs())
                    {
                        format!("not used for {} h", now.saturating_sub(s.used) / 3600)
                    } else {
                        continue;
                    };
                    let mut removed = false;
                    let mut error = None;
                    if !dry_run {
                        let on = crate::remote::RanOn {
                            pool: pool.clone(),
                            node: node.clone(),
                        };
                        match crate::remote::purge_slot(&self.rt, &on, &repo, &s.lane) {
                            Ok(()) => removed = true,
                            Err(e) => error = Some(e),
                        }
                    }
                    out.push(Pruned {
                        place: if pool == devstack_node::member::DEFAULT_POOL {
                            n.name.clone()
                        } else {
                            format!("{pool}:{}", n.name)
                        },
                        lane: s.lane,
                        bytes: Some(s.bytes),
                        why,
                        removed,
                        error,
                    });
                }
            }
        }
        // What `prepare` made, kept in this machine's store (ADR-0062): an
        // entry no copy used for `older_than`. Copies hold their own.
        if let Some(older_than) = older_than {
            for s in self.main.store().sweep(older_than, dry_run) {
                out.push(Pruned {
                    place: "this machine".into(),
                    lane: format!("store: {}", s.service),
                    bytes: Some(s.bytes),
                    why: format!(
                        "what `prepare` made ({}), not used for {} h",
                        s.key,
                        s.idle.as_secs() / 3600
                    ),
                    removed: s.removed,
                    error: s.error,
                });
            }
        }
        crate::diag::info(
            "lanes_pruned",
            serde_json::json!({ "dry_run": dry_run, "found": out.len(),
                "removed": out.iter().filter(|p| p.removed).count() }),
        );
        out
    }

    /// Stop and remove the containers of lane `name` here, keeping their
    /// volumes: what a lane leaves on this machine when it goes to a node
    /// (left over from a local start), which would hold its ports here.
    fn stop_local_infra(&self, name: &str) {
        let docker = devstack_compose::Docker::default();
        for (g, cfg) in &self.main.stack.compose {
            let group = devstack_compose::Group {
                name: g.clone(),
                project: format!("{}-{name}", cfg.project),
                files: Vec::new(),
                env_files: Vec::new(),
                networks: Vec::new(),
                services: Vec::new(),
                dir: self.main.repo.clone(),
                // No files: nothing for `DEVSTACK_PORT_*` or `DEVSTACK_LANE*` to fill in.
                env: Default::default(),
            };
            if let Err(e) = docker.down(&group) {
                crate::diag::warn(
                    "lane_infra_not_stopped",
                    serde_json::json!({ "lane": name, "group": g, "error": e.to_string() }),
                );
            }
        }
    }

    /// Remove the containers, volumes and networks of lane `name`
    /// (ADR-0030), also of a lane not started now: Compose finds a project
    /// by its name alone.
    /// Remove the lane's Compose projects here: `Err` names the groups
    /// whose removal failed (their containers or volumes are still there).
    fn remove_infra(&self, name: &str) -> Result<(), String> {
        let docker = devstack_compose::Docker::default();
        let mut failed = Vec::new();
        for (g, cfg) in &self.main.stack.compose {
            let group = devstack_compose::Group {
                name: g.clone(),
                project: format!("{}-{name}", cfg.project),
                files: Vec::new(),
                env_files: Vec::new(),
                networks: cfg.networks.iter().map(|n| format!("{n}-{name}")).collect(),
                services: Vec::new(),
                dir: self.main.repo.clone(),
                // No files: nothing for `DEVSTACK_PORT_*` or `DEVSTACK_LANE*` to fill in.
                env: Default::default(),
            };
            if let Err(e) = docker.remove(&group) {
                crate::diag::warn(
                    "lane_infra_not_removed",
                    serde_json::json!({ "lane": name, "group": g, "error": e.to_string() }),
                );
                failed.push(format!("{g}: {e}"));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// The main stack file's path relative to the repository: where a
    /// worktree has its own (ADR-0055 point 1).
    fn stack_file_rel(&self) -> PathBuf {
        let file = self.main.stack_file();
        file.strip_prefix(&self.main.repo)
            .or_else(|_| file.strip_prefix(devstack_proc::long_path(&self.main.repo)))
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from(file.file_name().unwrap_or("devstack.toml".as_ref())))
    }

    /// The lane's own stack file when its branch has one (it may have changed
    /// it), else the main working tree's (ADR-0055).
    fn stack_file_of(&self, repo: &Path) -> PathBuf {
        let own = repo.join(self.stack_file_rel());
        if own.is_file() {
            own
        } else {
            self.main.stack_file().to_path_buf()
        }
    }

    /// What a lane on a node takes along besides its worktree's files:
    /// `[lanes] sync`, and the stack file, from the main tree while the
    /// worktree has none of its own (ADR-0055 point 2).
    fn synced(&self) -> Vec<String> {
        let mut extra = self.main.stack.lanes.sync.clone();
        extra.push(self.stack_file_rel().to_string_lossy().replace('\\', "/"));
        extra
    }

    /// Whether the worktree at `path` can run with `profile` (ADR-0041): its
    /// own stack file has it, and what it makes holds.
    pub fn check_profile(&self, path: &Path, profile: &str) -> Result<(), String> {
        let repo = devstack_proc::long_path(&std::path::absolute(path).map_err(|e| e.to_string())?);
        let file = self.stack_file_of(&repo);
        let stack = devstack_config::load(&file).map_err(|e| e.to_string())?;
        stack.with_profile(profile).map(|_| ()).map_err(|issues| {
            issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    fn open(&self, entry: &LaneEntry) -> Result<Arc<Lane>, String> {
        let repo = devstack_proc::long_path(&entry.path);
        if !repo.is_dir() {
            return Err(format!("{} is gone", repo.display()));
        }
        let file = self.stack_file_of(&repo);
        let stack = devstack_config::load(&file).map_err(|e| e.to_string())?;
        // The profile it was started with (ADR-0041).
        let profile = self.profiles.lock().unwrap().get(&entry.name).cloned();
        let stack = match profile {
            Some(p) => stack.with_profile(&p).map_err(|issues| {
                issues
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            })?,
            None => stack,
        };
        // The main stack's step: the free ranges were chosen with it.
        let offset = lane_offset(entry.number, self.main.stack.lanes.port_step)
            .ok_or_else(|| format!("lane number {} is beyond the port range", entry.number))?;
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
            offset,
            self.main.main_owner(),
            // One build job per daemon (ADR-0020): a lane that goes takes no
            // build server along that the main tree's build uses.
            Some(self.main.build_job()),
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
        sup.follow_rebuilds();
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
    pub(crate) fn views(&self) -> Vec<LaneView> {
        let missing = self.missing.lock().unwrap().clone();
        let map = self.map.lock().unwrap();
        let remote = self.remote.lock().unwrap();
        let now = crate::state::now_ms();
        let moves = {
            let mut m = self.moves.lock().unwrap();
            m.retain(|_, x| {
                x.failed_at
                    .is_none_or(|t| now.saturating_sub(t) < MOVE_ERROR_FOR_MS)
            });
            m.clone()
        };
        let services = self.main.stack.services.len();
        let forgetting = self.forgetting.lock().unwrap().clone();
        let stack_file = self.stack_file_rel();
        self.known
            .lock()
            .unwrap()
            .values()
            .map(|k| {
                let lane = map.get(&k.entry.name);
                let on_node = remote.get(&k.entry.name);
                let node_state = on_node.map(|r| r.state());
                let (running, services, containers_running, containers) = match (lane, &node_state)
                {
                    (Some(l), _) => {
                        let v = l.sup.view();
                        let n = v.services.iter().filter(|s| s.status.is_active()).count();
                        let (cr, c) = crate::state::containers_running(&v.infra);
                        (n, v.services.len(), cr, c)
                    }
                    (None, Some(st)) => (
                        st.running,
                        if st.services > 0 {
                            st.services
                        } else {
                            services
                        },
                        st.containers_running,
                        st.containers,
                    ),
                    (None, _) => (0, services, 0, 0),
                };
                LaneView {
                    name: k.entry.name.clone(),
                    path: k.entry.path.clone(),
                    branch: k.branch.clone(),
                    number: k.entry.number,
                    port_offset: lane_offset(k.entry.number, self.main.stack.lanes.port_step)
                        .unwrap_or(0),
                    active: lane.is_some() || node_state.as_ref().is_some_and(|st| !st.ended),
                    owns_main: lane.is_some_and(|l| l.sup.owns_main())
                        || (on_node.is_some()
                            && self.main.served_by().as_deref() == Some(k.entry.name.as_str())),
                    running,
                    services,
                    containers_running,
                    containers,
                    node: on_node.map(|r| r.node.name.clone()),
                    pool: on_node.map(|r| r.pool.clone()),
                    shared_with: self.share_of(&k.entry.name),
                    profile: self.profiles.lock().unwrap().get(&k.entry.name).cloned(),
                    node_error: node_state.as_ref().and_then(|st| st.error()),
                    not_forwarded: node_state
                        .as_ref()
                        .map(|st| st.not_forwarded.clone())
                        .unwrap_or_default(),
                    not_forwarded_reason: node_state
                        .as_ref()
                        .and_then(|st| st.not_forwarded_reason.clone()),
                    node_warning: on_node
                        .and_then(|r| r.warning.clone())
                        .or_else(|| moves.get(&k.entry.name).and_then(|m| m.warning.clone())),
                    moving: moves.get(&k.entry.name).cloned(),
                    removing: forgetting.contains(&k.entry.name),
                    main_stack_file: k.entry.path.is_dir()
                        && !k.entry.path.join(&stack_file).is_file(),
                    notices: {
                        let mut n = lane.map(|l| l.sup.notices()).unwrap_or_default();
                        if let Some(t) = missing.get(&k.entry.name)
                            && !n.iter().any(|x| x.id == "worktree_missing")
                        {
                            let since = now.saturating_sub(t.elapsed().as_millis());
                            n.push(missing_notice(&k.entry.path, since));
                        }
                        n
                    },
                }
            })
            .collect()
    }

    /// Start the lane of the worktree at `path` (`devstack up` in it): its
    /// supervisor is set up; `/v1/lanes/<name>/v1/up` starts its services.
    pub fn activate(&self, path: &Path, profile: Option<&str>) -> Result<Arc<Lane>, String> {
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
            let runs = l.sup.stack.profile.as_ref().map(|p| p.name.as_str());
            if profile.is_some() && runs != profile {
                return Err(format!(
                    "lane `{name}` runs {}: `devstack lane stop {name}` first",
                    runs.map_or("without a profile".to_string(), |p| format!(
                        "with the profile `{p}`"
                    ))
                ));
            }
            return Ok(l.clone());
        }
        if let Some(r) = self.remote.lock().unwrap().get(&name) {
            return Err(format!(
                "lane `{name}` runs on {}: `devstack lane stop {name}` first",
                r.node.name
            ));
        }
        match profile {
            Some(p) => self
                .profiles
                .lock()
                .unwrap()
                .insert(name.clone(), p.to_string()),
            None => self.profiles.lock().unwrap().remove(&name),
        };
        if !self.pending.lock().unwrap().insert(name.clone()) {
            return Err(format!("lane `{name}` is being started"));
        }
        // Forgotten (its worktree went) between the look-up and now.
        if !self.known.lock().unwrap().contains_key(&name) {
            self.pending.lock().unwrap().remove(&name);
            return Err(format!("lane `{name}` is gone: its worktree was removed"));
        }
        let t0 = std::time::Instant::now();
        let result = (|| {
            // Its range taken by another process since it was found: a new one.
            let step = self.main.stack.lanes.port_step;
            let mut entry = entry;
            let busy = ports(&self.main.stack).iter().any(|p| {
                lane_offset(entry.number, step)
                    .and_then(|off| p.checked_add(off))
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
                // `ms`: opening the lane; its services' starts have lines of their own.
                serde_json::json!({ "lane": name, "worktree": entry.path, "number": entry.number,
                    "place": "local", "ms": t0.elapsed().as_millis() as u64 }),
            );
            Ok(lane)
        })();
        self.pending.lock().unwrap().remove(&name);
        if result.is_ok() {
            // Started here: a start on a node that failed before is over.
            self.forget_failed(Some(&name));
            self.announce();
        }
        result
    }

    /// `devstack lane stop`: the lane's services stop and it is no longer
    /// started; its snapshot and builds stay for the next start. The worktree
    /// is git's business and stays as it is.
    pub fn stop(&self, name: &str) -> Result<(), String> {
        if self.main.served_by().as_deref() == Some(name) && self.remote(name).is_some() {
            return Err(format!(
                "lane `{name}` serves the main slot: `devstack promote --lane main` first"
            ));
        }
        let t0 = std::time::Instant::now();
        // A profile holds while the lane runs (ADR-0041).
        self.profiles.lock().unwrap().remove(name);
        // Stopped: a start or move that failed before is no longer news.
        self.forget_failed(Some(name));
        let remote = self.remote.lock().unwrap().remove(name);
        if let Some(r) = remote {
            let node = r.node.name.clone();
            self.stop_remote(r);
            crate::diag::info(
                "lane_stopped",
                serde_json::json!({ "lane": name, "place": "remote", "node": node,
                    "ms": t0.elapsed().as_millis() as u64 }),
            );
            self.announce();
            return Ok(());
        }
        let lane = self.started(name)?;
        if lane.sup.owns_main() {
            return Err(format!(
                "lane `{name}` serves the main slot: `devstack promote --lane main` first"
            ));
        }
        // Its containers too: their memory is what a stopped lane gives back
        // (ADR-0030). Volumes stay for the next start.
        lane.sup.down(true);
        let removed = self.map.lock().unwrap().remove(name);
        if let Some(l) = removed {
            l.sup.close();
        }
        crate::diag::info(
            "lane_stopped",
            serde_json::json!({ "lane": name, "place": "local",
                "ms": t0.elapsed().as_millis() as u64 }),
        );
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
        // Taking it would change the owner and move nothing (#184).
        if !main.main_slot_moves() {
            return Err(
                "this stack has no services on main ports: Compose containers keep their own ports unless `[compose.<group>] main_ports` names them (ADR-0048)"
                    .into(),
            );
        }
        // A lane on a node (ADR-0025 point 4): on a dedicated node only; its
        // services answer here on their lane ports, forwarded to the node.
        let remote = self.remote(&t);
        if let Some(r) = &remote {
            if r.state().ended {
                return Err(format!("lane `{t}` on {} is over", r.node.name));
            }
            let dedicated = crate::remote::local_member(&r.pool)
                .nodes()
                .ok()
                .and_then(|ns| ns.into_iter().find(|n| n.id == r.node.id))
                .and_then(|n| n.report)
                .is_some_and(|rep| rep.dedicated);
            if !dedicated {
                return Err(format!(
                    "lane `{t}` runs on {}, which is no dedicated node (a server): the main slot goes to a lane there only (ADR-0025 point 4)",
                    r.node.name
                ));
            }
        }
        let views = match &remote {
            Some(_) => Vec::new(),
            None => self.started(&t)?.sup.view().services,
        };
        let offset = self
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
            })?,
            None => views
                .iter()
                .find(|v| v.name == s && v.status.is_active())
                .and_then(|v| v.port.map(|p| (p, v.build_id.clone()))),
        };
        // A proxied service the lane does not run (stopped, failed) moves
        // too: its main port answers once it runs in the lane, as every
        // proxied service's does (ADR-0024 point 5). Refusing kept the main
        // ports from a lane that ran all but one service.
        let lane_port = |s: &str| {
            main.stack
                .services
                .get(s)
                .and_then(|svc| svc.port)
                .and_then(|p| p.checked_add(offset))
        };
        let not_running: Vec<String> = main
            .proxied_services()
            .into_iter()
            .filter(|s| active(s).is_none())
            .map(|s| {
                match views
                    .iter()
                    .find(|v| v.name == s)
                    .map(|v| status_text(&v.status))
                {
                    Some(st) => format!("{s} ({st})"),
                    None => s,
                }
            })
            .collect();
        let prev = current
            .as_deref()
            .and_then(|c| self.map.lock().unwrap().get(c).cloned());
        let prev_remote = current.as_deref().and_then(|c| self.remote(c));

        // 1. Free the fixed ports. Forwards to the previous lane end; the main
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
                    ho.stopped.insert(s.clone(), main.build_of(&s));
                }
                let _ = main.stop(&s);
            }
        }
        let mut back_home = Vec::new();
        for s in main.unproxied_services() {
            if active(&s).is_some() {
                if main.service_active(&s) {
                    ho.stopped.insert(s.clone(), main.build_of(&s));
                }
                let _ = main.stop(&s);
            } else if let Some(build) = ho.stopped.remove(&s) {
                // The previous lane ran it, this one does not: it runs in the
                // main tree again.
                back_home.push((s, build));
            }
        }

        // 2. Point them at the lane. A failure gives everything back.
        let moved = (|| {
            for s in main.proxied_services() {
                let (port, build) = match active(&s) {
                    Some(a) => a,
                    None => (
                        lane_port(&s)
                            .ok_or_else(|| format!("{s}: its lane port is beyond 65535"))?,
                        None,
                    ),
                };
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
            drop(handing);
            let back = self.give_back(&mut ho);
            return Err(match back {
                Ok(()) => format!("{e}; the main working tree serves the main ports again"),
                Err(b) => format!("{e}; giving the main ports back: {b}"),
            });
        }

        // The containers' fixed ports devstack holds go too (ADR-0048).
        main.hold_for_lane(Some(offset));

        // 3. The new owner, and the workers with it.
        main.set_main_owner(Some(t.clone()));
        if let Some(p) = &prev {
            for s in p.sup.running_worker_services() {
                let _ = p.sup.relaunch(&s);
            }
        } else if let Some(r) = &prev_remote {
            let _ = r.set_main(&self.rt, false);
        } else {
            for s in main.running_worker_services() {
                let _ = main.relaunch(&s);
            }
        }
        let mut failed = Vec::new();
        match &remote {
            Some(r) => {
                if let Err(e) = r.set_main(&self.rt, true) {
                    failed.push(format!("the workers on {}: {e}", r.node.name));
                }
            }
            None => {
                let lane = self.started(&t)?;
                for s in lane.sup.running_worker_services() {
                    let _ = lane.sup.relaunch(&s);
                }
            }
        }
        for (s, build) in back_home {
            if let Err(e) = main.start_build(&s, build) {
                failed.push(format!("{s}: {e}"));
            }
        }
        crate::diag::info(
            "main_slot",
            serde_json::json!({ "lane": t, "not_running": not_running }),
        );
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
        if let Some(r) = main.served_by().and_then(|c| self.remote(&c)) {
            let _ = r.set_main(&self.rt, false);
        }
        for s in main.forwarded_services() {
            let _ = main.tcp_forward(&s, None);
        }
        main.set_main_owner(None);
        main.hold_for_lane(None);
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
            if let Some(build) = ho.stopped.remove(&s)
                && let Err(e) = main.start_build(&s, build)
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

    /// The lanes started, or starting, here and on nodes: while there is
    /// one, the main tree's `down` keeps the daemon they live in (ADR-0024).
    pub fn active(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = self.map.lock().unwrap().keys().cloned().collect();
        names.extend(self.remote.lock().unwrap().keys().cloned());
        names.extend(self.pending.lock().unwrap().iter().cloned());
        names.into_iter().collect()
    }

    /// Stop every lane (`down --all`): the one on the main ports gives them
    /// back first, then each stops as [`Self::stop`] does, its containers
    /// too, a lane on a node released. What did not stop, said.
    pub fn stop_all(&self) -> Vec<String> {
        let mut failed = Vec::new();
        if self.main.served_by().is_some()
            && let Err(e) = self.take_main("main")
        {
            failed.push(format!("the main ports: {e}"));
        }
        for name in self.active() {
            if let Err(e) = self.stop(&name) {
                failed.push(format!("lane `{name}`: {e}"));
            }
        }
        failed
    }

    /// Stop every lane's services (the daemon is stopping). For a restart
    /// (ADR-0052) what runs is noted for the next daemon first, and the
    /// lanes on nodes go on (renewed, forwarded) until [`Self::leave_all`].
    pub fn down_all(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let restart = self.restarting();
        if restart {
            *self.carry.lock().unwrap() = Some(self.carry_now());
        }
        let lanes: Vec<Arc<Lane>> = self.map.lock().unwrap().values().cloned().collect();
        for l in lanes {
            l.sup.down(false);
            l.sup.unlink_prepared();
        }
        if restart {
            return;
        }
        // A daemon that stops releases its lanes on nodes (ADR-0025 point 7).
        let remote: Vec<Arc<RemoteLane>> = std::mem::take(&mut *self.remote.lock().unwrap())
            .into_values()
            .collect();
        for r in remote {
            self.stop_remote(r);
        }
    }

    /// The last of a daemon that restarts (ADR-0052): the lanes on nodes are
    /// no longer followed from here, their leases renewed until now, and the
    /// word for the next daemon is written. Nothing without a restart.
    pub fn leave_all(&self) {
        let Some(mut carry) = self.carry.lock().unwrap().take() else {
            return;
        };
        for r in std::mem::take(&mut *self.remote.lock().unwrap()).into_values() {
            r.leave();
        }
        carry.written = crate::state::now_ms();
        let tmp = self.root_state.join(format!("{CARRY_FILE}.tmp"));
        let res = serde_json::to_vec_pretty(&carry)
            .map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|()| std::fs::rename(&tmp, self.carry_file()));
        if let Err(e) = res {
            crate::diag::warn(
                "carry_not_written",
                serde_json::json!({ "error": e.to_string() }),
            );
        }
    }

    /// The daemon is told to stop for a restart (ADR-0052).
    pub fn restart_next(&self) {
        self.restart
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn restarting(&self) -> bool {
        self.restart.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The lanes started here and on nodes (a start or move under way not
    /// counted): what a restart carries over.
    pub fn running_by_place(&self) -> (usize, usize) {
        let remote = self.remote.lock().unwrap();
        (
            self.map.lock().unwrap().len(),
            remote.values().filter(|r| !r.state().ended).count(),
        )
    }

    fn carry_file(&self) -> PathBuf {
        self.root_state.join(CARRY_FILE)
    }

    /// What runs, for the next daemon (ADR-0052 point 1): the lanes here
    /// with their profile, those on nodes with their lease, and who serves
    /// the main ports. A start or move under way is not carried.
    fn carry_now(&self) -> Carry {
        let profiles = self.profiles.lock().unwrap().clone();
        let mut lanes: Vec<CarriedLane> = self
            .map
            .lock()
            .unwrap()
            .keys()
            .map(|n| CarriedLane {
                name: n.clone(),
                profile: profiles.get(n).cloned(),
                node: None,
            })
            .collect();
        lanes.extend(
            self.remote
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, r)| !r.state().ended)
                .map(|(n, r)| CarriedLane {
                    name: n.clone(),
                    profile: profiles.get(n).cloned(),
                    node: Some(r.carried()),
                }),
        );
        for name in self.pending.lock().unwrap().iter() {
            crate::diag::warn(
                "lane_not_carried",
                serde_json::json!({ "lane": name, "why": "its start or move was under way" }),
            );
        }
        Carry {
            written: crate::state::now_ms(),
            stack: self.main.stack.stack.name.clone(),
            lanes,
            main: self.main.served_by(),
        }
    }

    /// What a daemon before this one left (ADR-0052): the lanes it carried
    /// over a restart, those on nodes first (their leases run out); then the
    /// lanes on nodes it followed when it ended without stopping them
    /// ([`Self::unfollowed`]); then the lanes here and the main slot.
    /// Blocking: on a thread of its own.
    pub fn take_up(self: &Arc<Self>) {
        let (lanes, main) = self
            .carried()
            .map(|c| (c.lanes, c.main))
            .unwrap_or_default();
        let (remote, local): (Vec<CarriedLane>, Vec<CarriedLane>) =
            lanes.into_iter().partition(|l| l.node.is_some());
        for l in remote {
            self.take_up_remote(l);
        }
        self.unfollowed();
        for l in local {
            self.take_up_local(l);
        }
        match main {
            Some(lane) => self.take_up_main(lane),
            None => self.announce(),
        }
    }

    /// The lanes on nodes a daemon before this one followed and neither
    /// stopped nor carried over (ADR-0052 point 6): it crashed, was killed,
    /// or the machine went down. Their leases are over by now (or released
    /// here, as a daemon that stops releases them, ADR-0025 point 7); each
    /// is said in the log and with the lane, so the lane does not just show
    /// as stopped.
    fn unfollowed(&self) {
        let names: Vec<String> = self.known.lock().unwrap().keys().cloned().collect();
        for name in names {
            let state = self.state_dir(&name);
            let Some(c) = crate::remote::followed(&state) else {
                continue;
            };
            if self.remote(&name).is_some_and(|r| r.lease == c.lease) {
                continue;
            }
            crate::remote::forget_followed(&state, &c.lease);
            if let Ok(client) = devstack_node::nodeclient::NodeClient::new(
                &crate::remote::local_member(&c.pool),
                c.node.clone(),
            ) {
                let _ = self.rt.block_on(client.release(&c.lease));
            }
            let node = c.node.name;
            crate::diag::warn(
                "remote_lane_unfollowed",
                serde_json::json!({ "lane": name, "node": node, "lease": c.lease }),
            );
            self.begin_move(&name, "start", &node, 1);
            self.finish(
                &name,
                Some(&format!(
                    "lane `{name}` ran on {node} until devstack here ended without stopping it \
                     (a crash, a restart of this machine); its lease there is over. \
                     `devstack up --on {node}` starts it again from its data kept there"
                )),
            );
        }
    }

    /// What a daemon before this one left running for a restart (ADR-0052
    /// point 2), if it is for this stack and recent. The word is read once.
    fn carried(&self) -> Option<Carry> {
        let path = self.carry_file();
        let bytes = std::fs::read(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        let carry: Carry = match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(e) => {
                crate::diag::warn(
                    "carry_unreadable",
                    serde_json::json!({ "error": e.to_string() }),
                );
                return None;
            }
        };
        let age = crate::state::now_ms().saturating_sub(carry.written);
        if carry.stack != self.main.stack.stack.name || age > CARRY_FOR.as_millis() {
            crate::diag::warn(
                "carry_ignored",
                serde_json::json!({ "age_ms": age as u64, "lanes": carry.lanes.len(),
                    "why": if carry.stack != self.main.stack.stack.name { "written for another stack" } else { "too old: the leases may be over" } }),
            );
            return None;
        }
        Some(carry)
    }

    /// The main slot goes back to the lane `lane` that held it at the
    /// restart (ADR-0052 point 2).
    fn take_up_main(self: &Arc<Self>, lane: String) {
        // The main tree starts beside this (`devstack up` asks for it):
        // the main slot moves once it did, so what the main tree hands
        // over is noted and comes back with `promote --lane main`.
        if !self.main.wait_upped(std::time::Duration::from_secs(900)) {
            crate::diag::warn(
                "main_slot_not_carried",
                serde_json::json!({ "lane": lane, "error": "the main tree did not start within 15 minutes" }),
            );
            if let Some(r) = self.remote(&lane) {
                let _ = r.set_main(&self.rt, false);
            }
            self.announce();
            return;
        }
        match self.take_main(&lane) {
            Ok(()) => crate::diag::info("main_slot_carried", serde_json::json!({ "lane": lane })),
            Err(e) => {
                // The lane on a node runs its workers no longer.
                if let Some(r) = self.remote(&lane) {
                    let _ = r.set_main(&self.rt, false);
                }
                crate::diag::warn(
                    "main_slot_not_carried",
                    serde_json::json!({ "lane": lane, "error": e }),
                );
            }
        }
        self.announce();
    }

    /// A lane not taken up after a restart: said in the log, and with the
    /// lane for a while (its `moving`, as a start that failed).
    fn not_carried(&self, name: &str, place: &str, to: &str, error: &str) {
        crate::diag::warn(
            "lane_not_carried",
            serde_json::json!({ "lane": name, "place": place, "node": (place == "remote").then_some(to), "error": error }),
        );
        if self.known.lock().unwrap().contains_key(name) {
            self.begin_move(name, "start", to, 1);
            self.finish(
                name,
                Some(&format!(
                    "lane `{name}` did not come back after the restart: {error}"
                )),
            );
        }
    }

    fn take_up_remote(&self, l: CarriedLane) {
        let t0 = std::time::Instant::now();
        let c = l.node.expect("partitioned: on a node");
        let node = c.node.name.clone();
        let entry = self
            .known
            .lock()
            .unwrap()
            .get(&l.name)
            .map(|k| k.entry.clone());
        let Some(entry) = entry else {
            // Its worktree went meanwhile: the lane goes with it (ADR-0024
            // point 7), its lease too.
            if let Ok(client) = devstack_node::nodeclient::NodeClient::new(
                &crate::remote::local_member(&c.pool),
                c.node.clone(),
            ) {
                let _ = self.rt.block_on(client.release(&c.lease));
            }
            crate::remote::forget_followed(&self.state_dir(&l.name), &c.lease);
            return self.not_carried(&l.name, "remote", &node, "its worktree is gone");
        };
        if let Some(p) = &l.profile {
            self.profiles
                .lock()
                .unwrap()
                .insert(l.name.clone(), p.clone());
        }
        let lease = c.lease.clone();
        let adopted = crate::remote::RemoteLane::adopt(
            &self.rt,
            &l.name,
            c,
            &devstack_proc::long_path(&entry.path),
            &self.main.repo,
            &self.synced(),
            self.sharing_of(&entry),
            &self.state_dir(&l.name).join("remote"),
        );
        match adopted {
            Ok(r) => {
                self.remote.lock().unwrap().insert(l.name.clone(), r);
                crate::diag::info(
                    "lane_carried",
                    serde_json::json!({ "lane": l.name, "place": "remote", "node": node,
                        "ms": t0.elapsed().as_millis() as u64 }),
                );
                self.announce();
            }
            Err(e) => {
                self.profiles.lock().unwrap().remove(&l.name);
                // Said here: not again as a lane nobody followed.
                crate::remote::forget_followed(&self.state_dir(&l.name), &lease);
                self.not_carried(&l.name, "remote", &node, &e);
            }
        }
    }

    fn take_up_local(&self, l: CarriedLane) {
        let t0 = std::time::Instant::now();
        let entry = self
            .known
            .lock()
            .unwrap()
            .get(&l.name)
            .map(|k| k.entry.clone());
        let Some(LaneEntry { path, number, .. }) = entry else {
            return self.not_carried(&l.name, "local", "local", "its worktree is gone");
        };
        // Its ports stay its own: the old daemon's processes of the lane may
        // hold them a moment after it exited (Windows), and a start that
        // finds them taken moves the lane to another range.
        let step = self.main.stack.lanes.port_step;
        let mine: Vec<u16> = lane_offset(number, step)
            .map(|off| {
                ports(&self.main.stack)
                    .iter()
                    .filter_map(|p| p.checked_add(off))
                    .collect()
            })
            .unwrap_or_default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while mine.iter().any(|p| crate::supervisor::port_in_use(*p))
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        // From the artifacts it ran (ADR-0016 point 4), as `devstack up` in
        // its worktree: a service that fails shows as failed, as after any
        // `up`, and the lane runs the rest.
        match self.activate(&path, l.profile.as_deref()) {
            Ok(lane) => {
                let failed = lane.sup.up().err();
                crate::diag::info(
                    "lane_carried",
                    serde_json::json!({ "lane": l.name, "place": "local", "failed": failed,
                        "ms": t0.elapsed().as_millis() as u64 }),
                );
            }
            Err(e) => self.not_carried(&l.name, "local", "local", &e),
        }
    }

    /// What a start or move of `name` reports its steps to: the lane's
    /// `moving`, announced to every client at each change.
    fn stepper<'a>(
        &'a self,
        name: &str,
        kind: &'static str,
        to: &str,
        steps: u8,
    ) -> impl Fn(u8, &str) + Send + Sync + 'a {
        self.begin_move(name, kind, to, steps);
        self.step_fn(name)
    }

    /// A start or move of `name` begins: its `moving` is shown from now on.
    fn begin_move(&self, name: &str, kind: &'static str, to: &str, steps: u8) {
        let since = crate::state::now_ms();
        self.moves.lock().unwrap().insert(
            name.to_string(),
            LaneMove {
                kind,
                to: to.to_string(),
                step: 0,
                steps,
                what: "preparing".into(),
                since,
                error: None,
                failed_at: None,
                warning: None,
                phases: Vec::new(),
                phase_since: since,
                phase: "preparing".into(),
            },
        );
        self.announce();
    }

    /// What a start or move of `name` that has begun reports its steps to.
    fn step_fn<'a>(&'a self, name: &str) -> impl Fn(u8, &str) + Send + Sync + 'a {
        let name = name.to_string();
        move |step: u8, what: &str| {
            if let Some(m) = self.moves.lock().unwrap().get_mut(&name) {
                if m.step == step && m.what == what {
                    return;
                }
                let phase = phase_of(what);
                if phase != m.phase {
                    let now = crate::state::now_ms();
                    let done = std::mem::replace(&mut m.phase, phase);
                    m.phases.push(MovePhase {
                        phase: done,
                        ms: now.saturating_sub(m.phase_since) as u64,
                    });
                    m.phase_since = now;
                }
                m.step = step;
                m.what = what.to_string();
            }
            self.announce();
        }
    }

    /// The start or move of `name` is over: gone when it worked, kept with
    /// its reason for a while when it did not.
    fn finish(&self, name: &str, error: Option<&String>) {
        {
            let mut moves = self.moves.lock().unwrap();
            match error {
                None => {
                    moves.remove(name);
                }
                Some(e) => {
                    if let Some(m) = moves.get_mut(name) {
                        m.error = Some(e.clone());
                        m.failed_at = Some(crate::state::now_ms());
                    }
                }
            }
        }
        self.announce();
    }

    /// Forget the start or move of `name` (`None`: of every lane) that
    /// failed: the lane started another way, or it or the stack stopped.
    pub fn forget_failed(&self, name: Option<&str>) {
        let gone = {
            let mut moves = self.moves.lock().unwrap();
            let before = moves.len();
            moves.retain(|n, m| m.error.is_none() || name.is_some_and(|x| x != n));
            moves.len() != before
        };
        if gone {
            self.announce();
        }
    }

    /// Release a lane on a node, on a thread of its own: the call blocks on
    /// the node, and this may run on the runtime's.
    fn stop_remote(&self, r: Arc<RemoteLane>) {
        let rt = self.rt.clone();
        let _ = std::thread::spawn(move || r.stop(&rt)).join();
    }

    /// A started lane that runs on a node.
    pub fn remote(&self, name: &str) -> Option<Arc<RemoteLane>> {
        self.remote.lock().unwrap().get(name).cloned()
    }

    /// Start the lane of the worktree at `path` on the node `on` (`devstack
    /// up --on`, ADR-0025): leased, synced and started there, followed from
    /// here. `need` is what it is expected to take.
    ///
    /// What can be told at once is answered at once: no such worktree, a
    /// clash with where the lane runs, no lease (no node, the coordinator or
    /// the node refuses). The sync and the start on the node, minutes on a
    /// first start, go on in the background ([`RemoteStart::Starting`]); the
    /// lane's `moving` shows how far they got, and is gone once it runs or
    /// carries the error if it failed (B1: a request open for minutes
    /// is cut off by whatever proxy or client has a shorter timeout).
    pub fn activate_remote(
        self: &Arc<Self>,
        path: &Path,
        on: &str,
        need: crate::remote::Expected,
        profile: Option<&str>,
    ) -> Result<RemoteStart, String> {
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        if same_tree(&path, &self.main.repo) {
            return Err("that is the main working tree: it runs on this machine".into());
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
        if self.map.lock().unwrap().contains_key(&name) {
            return Err(format!(
                "lane `{name}` runs on this machine: `devstack lane stop {name}` first"
            ));
        }
        // A lane whose lease is over starts afresh.
        self.remote
            .lock()
            .unwrap()
            .retain(|n, r| n != &name || !r.state().ended);
        if let Some(r) = self.remote.lock().unwrap().get(&name) {
            return if r.is_at(on) {
                Ok(RemoteStart::Running(name))
            } else {
                Err(format!(
                    "lane `{name}` runs on {} already: `devstack lane move {name} --to {on}` moves it",
                    r.node.name
                ))
            };
        }
        if !self.pending.lock().unwrap().insert(name.clone()) {
            // The same start asked again (a client that was interrupted and
            // runs `up --on` once more): it follows the one under way.
            let same = self.moves.lock().unwrap().get(&name).is_some_and(|m| {
                m.kind == "start" && m.error.is_none() && m.to.eq_ignore_ascii_case(on)
            });
            return if same {
                Ok(RemoteStart::Starting(name))
            } else {
                Err(format!("lane `{name}` is being started or moved"))
            };
        }
        match profile {
            Some(p) => self
                .profiles
                .lock()
                .unwrap()
                .insert(name.clone(), p.to_string()),
            None => self.profiles.lock().unwrap().remove(&name),
        };
        self.begin_move(&name, "start", on, 3);
        let leased = {
            let step = self.step_fn(&name);
            self.with_spec(&entry, on, &need, true, &step, |spec| {
                RemoteLane::lease(&self.rt, &spec)
            })
        };
        let leased = match leased {
            Ok(l) => l,
            Err(e) => {
                self.start_failed(&name, on, &e);
                return Err(e);
            }
        };
        // Another devstack there: said with the lane while it starts, so
        // the clients following the start after the 202 see it.
        if let Some(w) = leased.warning() {
            if let Some(m) = self.moves.lock().unwrap().get_mut(&name) {
                m.warning = Some(w.to_string());
            }
            self.announce();
        }
        let lanes = self.clone();
        let (lane, to) = (name.clone(), on.to_string());
        let spawned = std::thread::Builder::new()
            .name(format!("lane-start-{name}"))
            .spawn(move || lanes.launch_remote(&entry, &to, need, leased));
        if let Err(e) = spawned {
            // The lease expires on the node by itself.
            let e = format!("lane `{lane}`: {e}");
            self.start_failed(&lane, on, &e);
            return Err(e);
        }
        Ok(RemoteStart::Starting(name))
    }

    /// The background part of [`Lanes::activate_remote`]: sync, start on
    /// the node, follow. Its outcome is the lane's: running on the node, or
    /// `moving` with the error.
    fn launch_remote(
        &self,
        entry: &LaneEntry,
        on: &str,
        need: crate::remote::Expected,
        leased: crate::remote::Leased,
    ) {
        let name = &entry.name;
        // Containers of an earlier local start would hold its ports here.
        self.stop_local_infra(name);
        let result = {
            let step = self.step_fn(name);
            self.with_spec(entry, on, &need, true, &step, |spec| {
                RemoteLane::launch(&self.rt, spec, leased)
            })
        };
        match result {
            Ok(lane) => {
                // Listed as running on the node before `moving` goes, so a
                // client never sees it neither starting nor started.
                self.remote.lock().unwrap().insert(name.clone(), lane);
                self.finish(name, None);
                self.pending.lock().unwrap().remove(name);
            }
            Err(e) => self.start_failed(name, on, &e),
        }
    }

    /// A start on a node failed: said in the log, and in the lane's
    /// `moving` for a while.
    fn start_failed(&self, name: &str, on: &str, error: &String) {
        crate::diag::warn(
            "remote_lane_not_started",
            serde_json::json!({ "lane": name, "on": on, "error": error }),
        );
        self.finish(name, Some(error));
        self.pending.lock().unwrap().remove(name);
    }

    /// Whom the lane `name` is shared with: what its owner said, else the
    /// stack's default.
    fn share_of(&self, name: &str) -> Vec<String> {
        self.shares
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.main.stack.lanes.share.clone())
    }

    /// What the coordinator is told of the lane `entry` shared: whom with,
    /// and its services with a port (on the node, and here).
    fn sharing_of(&self, entry: &LaneEntry) -> Option<devstack_node::coordinator::Sharing> {
        let with = self.share_of(&entry.name);
        if with.is_empty() {
            return None;
        }
        let offset = lane_offset(entry.number, self.main.stack.lanes.port_step)?;
        let services = shared_services(
            &self.main.stack,
            offset,
            &entry.name,
            &self.main.published_fixed(),
        );
        Some(devstack_node::coordinator::Sharing::new(with, services))
    }

    /// Share the lane `name` with `with` (member ids, tags, `*`), or with
    /// nobody (empty), ADR-0035; they may use it and view it (ADR-0040). A
    /// lane on a node tells the coordinator at once; one not on a node keeps
    /// it for when it goes to one.
    pub fn share_lane(&self, name: &str, with: Vec<String>) -> Result<(), String> {
        let entry = self
            .known
            .lock()
            .unwrap()
            .get(name)
            .map(|k| k.entry.clone())
            .ok_or_else(|| format!("no lane `{name}` (see `devstack lane list`)"))?;
        let with: Vec<String> = with
            .into_iter()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();
        {
            let mut shares = self.shares.lock().unwrap();
            shares.insert(name.to_string(), with);
            let path = self.root_state.join("lane-shares.json");
            let _ = std::fs::create_dir_all(&self.root_state);
            let _ = std::fs::write(
                &path,
                serde_json::to_vec_pretty(&*shares).expect("shares serialise"),
            );
        }
        let r = if let Some(remote) = self.remote(name) {
            remote.share(self.sharing_of(&entry))
        } else {
            Ok(())
        };
        self.announce();
        r
    }

    /// Lease, sync and start the lane of `entry` on `on`; `forward`: listen
    /// on its ports here at once.
    fn start_remote(
        &self,
        entry: &LaneEntry,
        on: &str,
        need: crate::remote::Expected,
        forward: bool,
        step: &(dyn Fn(u8, &str) + Send + Sync),
    ) -> Result<Arc<RemoteLane>, String> {
        self.with_spec(entry, on, &need, forward, step, |spec| {
            RemoteLane::start(&self.rt, spec)
        })
    }

    /// `f` with what the lane of `entry` on `on` is to be.
    fn with_spec<T>(
        &self,
        entry: &LaneEntry,
        on: &str,
        need: &crate::remote::Expected,
        forward: bool,
        step: &(dyn Fn(u8, &str) + Send + Sync),
        f: impl FnOnce(crate::remote::RemoteSpec<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let stack_file = self.stack_file_rel().to_string_lossy().replace('\\', "/");
        let synced = self.synced();
        let scratch = self.state_dir(&entry.name).join("remote");
        let profile = self.profiles.lock().unwrap().get(&entry.name).cloned();
        // What the lane's own stack file declares it needs at least, with
        // its profile (ADR-0049 point 5); the main tree's where it does not
        // load.
        let declared = devstack_config::load(&entry.path.join(&stack_file))
            .map(|s| s.lane_need(profile.as_deref()))
            .unwrap_or_else(|_| self.main.stack.lane_need(profile.as_deref()));
        let worktree = devstack_proc::long_path(&entry.path);
        let offset = lane_offset(entry.number, self.main.stack.lanes.port_step)
            .ok_or_else(|| format!("lane `{}`: its ports are beyond 65535", entry.name))?;
        f(crate::remote::RemoteSpec {
            name: &entry.name,
            repo: &self.main.stack.stack.name,
            worktree: &worktree,
            main: &self.main.repo,
            extra: &synced,
            sharing: self.sharing_of(entry),
            stack_file: &stack_file,
            on,
            need,
            floor: crate::remote::floor_of(&declared),
            scratch: &scratch,
            // The lane runs on its lane ports there as well: the same
            // port on both ends.
            ports: ports(&self.main.stack)
                .into_iter()
                .filter_map(|p| p.checked_add(offset))
                .map(|p| (p, p))
                .collect(),
            port_offset: offset,
            forward,
            step,
            profile: profile.as_deref(),
        })
    }

    /// The services a lane on a node must have ready before it takes over:
    /// every autostart service that runs in lanes and on Linux.
    fn expected_remote(&self) -> Vec<String> {
        self.main
            .stack
            .services
            .iter()
            .filter(|(_, s)| {
                s.autostart
                    && s.lanes
                    && (s.platforms.is_empty()
                        || s.platforms
                            .contains(&devstack_config::model::Platform::Linux))
            })
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// `devstack lane move <lane> --to <node|local>` (ADR-0025 point 10): a
    /// cold start at the target, made before the old placement is broken.
    /// The target is leased, synced and started, and only once its services
    /// are ready do the lane's ports here switch to it and the old placement
    /// go. Back to this machine the ports cannot be held twice: they are
    /// handed over first, the lane on the node keeps running until the local
    /// one is ready, and comes back if it does not get there.
    pub fn move_lane(
        &self,
        name: &str,
        to: &str,
        need: crate::remote::Expected,
    ) -> Result<(), String> {
        let here = matches!(to, "local" | "here" | "this-machine");
        let entry = self
            .known
            .lock()
            .unwrap()
            .get(name)
            .map(|k| k.entry.clone())
            .ok_or_else(|| format!("no lane `{name}` (see `devstack lane list`)"))?;
        let local = self.map.lock().unwrap().get(name).cloned();
        let remote = self.remote(name);
        if local.is_none() && remote.is_none() {
            return Err(format!(
                "lane `{name}` is not started: `devstack up --on {to}` in its worktree starts it there"
            ));
        }
        if local.as_ref().is_some_and(|l| l.sup.owns_main())
            || (remote.is_some() && self.main.served_by().as_deref() == Some(name))
        {
            return Err(format!(
                "lane `{name}` serves the main slot: `devstack promote --lane main` first"
            ));
        }
        if here && local.is_some() {
            return Ok(());
        }
        if let Some(r) = &remote
            && !here
            && r.is_at(to)
        {
            return Ok(());
        }
        if !self.pending.lock().unwrap().insert(name.to_string()) {
            return Err(format!("lane `{name}` is being started or moved"));
        }
        let t0 = std::time::Instant::now();
        let result = if here {
            let r = remote.expect("checked: it runs on a node");
            let step = self.stepper(name, "move", "local", 3);
            self.move_here(&entry, r, &step)
        } else {
            let step = self.stepper(name, "move", to, 5);
            self.move_to_node(&entry, to, need, local, remote, &step)
        };
        self.finish(name, result.as_ref().err());
        self.pending.lock().unwrap().remove(name);
        match &result {
            Ok(()) => crate::diag::info(
                "lane_moved",
                serde_json::json!({ "lane": name, "to": to, "ms": t0.elapsed().as_millis() as u64 }),
            ),
            Err(e) => crate::diag::warn(
                "lane_not_moved",
                serde_json::json!({ "lane": name, "to": to, "error": e }),
            ),
        }
        self.announce();
        result
    }

    fn move_to_node(
        &self,
        entry: &LaneEntry,
        to: &str,
        need: crate::remote::Expected,
        local: Option<Arc<Lane>>,
        remote: Option<Arc<RemoteLane>>,
        step: &(dyn Fn(u8, &str) + Send + Sync),
    ) -> Result<(), String> {
        let name = &entry.name;
        let next = self.start_remote(entry, to, need, false, step)?;
        let expected = self.expected_remote();
        step(4, &format!("services: starting {}", expected.len()));
        let ready = self.rt.block_on(next.up_and_wait(
            &expected,
            std::time::Duration::from_secs(900),
            &|n, of| step(4, &format!("services: {n} of {of} ready")),
        ));
        if let Err(e) = ready {
            self.stop_remote(next);
            return Err(format!("lane `{name}` on {to}: {e}; it stays where it was"));
        }
        // Ready there: the old placement goes, the ports switch.
        step(5, "switching the ports here to it");
        if let Some(r) = remote {
            self.remote.lock().unwrap().remove(name);
            r.stop_forwarding();
            self.stop_remote(r);
        }
        if let Some(l) = local {
            l.sup.down(true);
            let removed = self.map.lock().unwrap().remove(name);
            if let Some(l) = removed {
                l.sup.close();
            }
        }
        // Whatever of the lane is left here (a local start's containers)
        // goes, so its ports here are free for the forwards.
        self.stop_local_infra(name);
        next.start_forwarding(&self.rt);
        self.remote.lock().unwrap().insert(name.clone(), next);
        Ok(())
    }

    fn move_here(
        &self,
        entry: &LaneEntry,
        r: Arc<RemoteLane>,
        step: &(dyn Fn(u8, &str) + Send + Sync),
    ) -> Result<(), String> {
        let name = &entry.name;
        step(1, "handing the ports over");
        r.stop_forwarding();
        self.remote.lock().unwrap().remove(name);
        self.pending.lock().unwrap().remove(name);
        step(2, "starting it here (build, infrastructure, services)");
        let profile = self.profiles.lock().unwrap().get(name).cloned();
        let started = self
            .activate(&entry.path, profile.as_deref())
            .and_then(|lane| {
                lane.sup.up().inspect_err(|_| {
                    lane.sup.down(true);
                    let removed = self.map.lock().unwrap().remove(name);
                    if let Some(l) = removed {
                        l.sup.close();
                    }
                })
            });
        self.pending.lock().unwrap().insert(name.clone());
        match started {
            Ok(_) => {
                step(3, "releasing its lease on the node");
                self.stop_remote(r);
                Ok(())
            }
            Err(e) => {
                // Back to the node it ran on, which never stopped.
                r.start_forwarding(&self.rt);
                self.remote.lock().unwrap().insert(name.clone(), r);
                Err(format!("lane `{name}` here: {e}; it stays on its node"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_repository_and_its_worktrees_are_its_trees() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| git(&repo, args).unwrap();
        g(&["init", "-q"]);
        g(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        let wt = d.path().join("wt");
        g(&["worktree", "add", "-q", &wt.to_string_lossy()]);
        assert!(worktree_of(&repo, &repo).is_some());
        assert!(worktree_of(&repo, &wt).is_some_and(|p| same_tree(&p, &wt)));
        assert!(worktree_of(&repo, &repo.join(".git")).is_none());
        assert!(worktree_of(&repo, d.path()).is_none());
    }

    #[test]
    fn the_state_of_a_forgotten_lane_is_swept() {
        let d = tempfile::tempdir().unwrap();
        for dir in ["lane-a", "lane-gone", "lane-lane-x", "lanes"] {
            std::fs::create_dir_all(d.path().join(dir).join("src")).unwrap();
        }
        std::fs::write(d.path().join("lane-shares.json"), "{}").unwrap();
        let known: BTreeSet<String> = ["a", "lane-x"].map(String::from).into();
        sweep_state_dirs(d.path(), &known);
        assert!(d.path().join("lane-a").is_dir());
        assert!(d.path().join("lane-lane-x").is_dir());
        assert!(d.path().join("lanes").is_dir());
        assert!(d.path().join("lane-shares.json").is_file());
        assert!(!d.path().join("lane-gone").exists());

        // Nothing holds it: removed at once (the list only stops a retry).
        let lanes = d.path().join("lanes.json");
        remove_state_dir("a", d.path().join("lane-a"), lanes);
        assert!(!d.path().join("lane-a").exists());
    }

    #[test]
    fn a_project_is_a_lanes_only_with_its_overlay_in_this_repository() {
        let state = Path::new("D:/work/shop/.devstack/lane-fix");
        let files = r"D:\work\shop\.devstack\lane-fix\compose\app.yml,D:/other.yml";
        assert!(among(files, state), "spelled with backslashes");
        assert!(among(
            "D:/work/shop/.devstack/lane-fix/compose/app.yml",
            state
        ));
        assert!(
            !among("D:/work/admin/compose.yml", state),
            "another repository's"
        );
        assert!(
            !among("D:/work/shop/.devstack/lane-fixed/compose/app.yml", state),
            "another lane's"
        );
    }

    #[test]
    fn a_lanes_links_move_with_its_ports() {
        assert_eq!(
            lane_url("http://localhost:8090", 200),
            "http://localhost:8290"
        );
        assert_eq!(
            lane_url("https://127.0.0.1:5100/ui?x=1", 100),
            "https://127.0.0.1:5200/ui?x=1"
        );
        assert_eq!(
            lane_url("http://[::1]:3000#top", 100),
            "http://[::1]:3100#top"
        );
        // The main tree, another host, no port: as written.
        assert_eq!(
            lane_url("http://localhost:8090", 0),
            "http://localhost:8090"
        );
        assert_eq!(
            lane_url("http://docs.example.com:8090/x", 100),
            "http://docs.example.com:8090/x"
        );
        assert_eq!(
            lane_url("https://localhost/ui", 100),
            "https://localhost/ui"
        );
        assert_eq!(
            lane_url("http://localhost:65500", 100),
            "http://localhost:65500"
        );
    }

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
    fn a_steps_text_names_its_phase() {
        assert_eq!(phase_of("sync: comparing 7177 files"), "sync");
        assert_eq!(
            phase_of("sync: 3 of 40 changed files sent (12 KiB)"),
            "sync"
        );
        assert_eq!(
            phase_of("on n1: infrastructure: app: starting containers"),
            "infrastructure: app: starting containers"
        );
        assert_eq!(phase_of("on n1: starting"), "starting");
        assert_eq!(phase_of("preparing"), "preparing");
    }

    #[test]
    fn a_shared_lane_names_its_lane_ports() {
        let stack = devstack_config::from_str(
            "version = 1\n[stack]\nname = \"t\"\n[services.web]\nrun = \"x\"\nport = 8091\n[services.job]\nkind = \"job\"\nrun = \"y\"\n",
            Path::new("t.toml"),
        )
        .unwrap();
        // On a node the lane listens where it does here: its lane ports.
        let s = shared_services(&stack, 200, "fix", &BTreeMap::new());
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(
            (s[0].name.as_str(), s[0].port, s[0].here),
            ("web", 8291, 8291)
        );
        assert!(shared_services(&stack, 65000, "fix", &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_shared_lane_names_its_web_containers() {
        let stack = devstack_config::from_str(
            r#"version = 1
[stack]
name = "t"
[compose.app]
project = "t"
files = ["compose.yml"]
services = ["map", "db", "admin", "mail", "two"]
shared = ["mail"]
urls = { db = "", admin = "https://localhost:9443/ui", two = "http://localhost:7001" }
[services.map]
run = "x"
port = 8080
"#,
            Path::new("t.toml"),
        )
        .unwrap();
        let published: BTreeMap<(String, String, u16), String> = [
            ("map", 80, "127.0.0.1:8081"),    // no entry: a web page, as linked
            ("db", 5432, "127.0.0.1:5432"),   // `""`: no link, not shared
            ("admin", 443, "0.0.0.0:9443"),   // its entry's scheme
            ("mail", 8025, "127.0.0.1:8025"), // the main tree's (`shared`)
            ("other", 80, "127.0.0.1:8090"),  // not started by the lane
            ("two", 7000, "127.0.0.1:7000"),  // not the port its link names
            ("two", 7001, "127.0.0.1:7001"),
        ]
        .into_iter()
        .map(|(s, t, a)| (("app".to_string(), s.to_string(), t), a.to_string()))
        .collect();
        let s = shared_services(&stack, 200, "fix", &published);
        let got: Vec<(&str, u16, &str)> = s
            .iter()
            .map(|s| (s.name.as_str(), s.port, s.scheme.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("map", 8280, "http"),
                ("admin", 9643, "https"),
                // A container named like a service of the stack file.
                ("app.map", 8281, "http"),
                ("two", 7201, "http"),
            ],
            "{s:?}"
        );
        assert!(s.iter().all(|s| s.here == s.port));
    }

    #[test]
    fn viewers_of_a_lane_join_its_sharing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("lane-shares.json"), r#"{"a": ["team:qa"]}"#).unwrap();
        std::fs::write(
            d.path().join("lane-views.json"),
            r#"{"a": ["team:qa", "bob"], "b": ["*"], "c": []}"#,
        )
        .unwrap();
        let shares = super::load_shares(d.path(), &["team:dev".to_string()]);
        assert_eq!(shares["a"], ["team:qa", "bob"]);
        assert_eq!(shares["b"], ["team:dev", "*"]);
        assert!(!shares.contains_key("c"));
        assert!(!d.path().join("lane-views.json").exists());
        assert_eq!(super::load_shares(d.path(), &[]), shares);
    }

    #[test]
    fn worktree_list_is_parsed() {
        let text = "worktree /r/main\nHEAD abc\nbranch refs/heads/main\n\n\
                    worktree /r/wt-a\nHEAD def\nbranch refs/heads/feature/a\n\n\
                    worktree /r/wt-b\nHEAD 123\ndetached\n\n\
                    worktree /r/gone\nHEAD 456\nbranch refs/heads/x\nprunable gitdir file points to non-existent location\n";
        let w = parse_worktrees(text);
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
