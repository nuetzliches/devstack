//! The share switch makes this machine a node (ADR-0113): with it on, the
//! hub runs the node agent for the pool and starts it again when it ends;
//! off, it ends it.
//!
//! - **Linux:** the agent runs inside the hub (`devstack_node::node::serve`
//!   on a thread of its own), with the machine's membership.
//! - **Windows:** lanes are Linux containers, so the agent runs in a WSL2
//!   distribution the user named, as root (`wsl.exe -d <distro> -u root --
//!   devstack node agent …`), with a copy of the machine's membership there
//!   (`/var/lib/devstack-node/<pool>`): one member per machine (point 2).
//!   The hub never sets the distribution up (point 3): it checks it and says
//!   what is missing, as commands and as a prompt for a coding agent.
//! - **macOS:** not yet.
//!
//! Off while other members' lanes run here (point 5): the agent goes on
//! without taking new lanes (`--no-new-lanes`) and ends once the last one
//! did. While the hub's agent runs, the hub does not report the machine
//! itself (#170).
//!
//! - `GET /hub/pool/share?pool=` carries the switch and the agent's state
//!   (`switch`, `host`), see `share.rs`
//! - `PUT /hub/pool/share?pool=` `{enabled, max_cpus?, max_memory?, distro?,
//!   port?}`: change the switch; acted on within a few seconds

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often the hub looks at each pool's switch and agent.
const EVERY: Duration = Duration::from_secs(5);
/// The agent's port unless the switch names another.
pub const DEFAULT_PORT: u16 = 4261;
/// An agent that ends sooner than this after its start failed: it is
/// started again only after [`BACKOFF`].
const SETTLE: Duration = Duration::from_secs(30);
const BACKOFF: Duration = Duration::from_secs(60);
/// How long a blocked switch's checks hold before they run again unasked:
/// on Windows each one starts `wsl.exe` and PowerShell, which keeps the WSL
/// VM awake.
const RECHECK: Duration = Duration::from_secs(60);
/// Why an agent ended, when it said nothing: inside the hub (Linux) its
/// lines go to the hub's log.
const ENDED: &str = if cfg!(target_os = "linux") {
    "the node agent ended: its lines are in the hub's log (hub.log in devstack's state folder)"
} else {
    "the node agent ended"
};
/// How long stopping the agent inside the hub waits for its blocking work.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const STOP_WAIT: Duration = Duration::from_secs(5);
/// Where the hub on Windows puts the membership's copy in the distribution.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WSL_DIR: &str = "/var/lib/devstack-node";
/// The Hyper-V firewall rule the setup names (`New-NetFirewallHyperVRule`).
#[cfg_attr(not(windows), allow(dead_code))]
pub const FIREWALL_RULE: &str = "devstack-node";

const MAC: &str = "A Mac cannot run other members' lanes yet.";

/// The switch of one pool, in `node-host.json` beside its membership. A file
/// of its own: the `share.json` of releases before 0.23 may still say
/// `enabled`, which must not start an agent now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Switch {
    #[serde(default)]
    pub enabled: bool,
    /// Caps on what the node's lanes take together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpus: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory: Option<u64>,
    /// Windows: the WSL distribution the agent runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// The agent's port; [`DEFAULT_PORT`] when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

fn switch_path(dir: &Path) -> PathBuf {
    dir.join("node-host.json")
}

impl Switch {
    pub fn load(dir: &Path) -> Self {
        std::fs::read(switch_path(dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            switch_path(dir),
            serde_json::to_vec_pretty(self).expect("switch serialises"),
        )
    }

    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }

    /// What an empty or impossible value would make of the agent.
    pub fn problem(&self) -> Option<String> {
        if self.max_cpus.is_some_and(|c| !c.is_finite() || c <= 0.0) {
            return Some("max_cpus: a number of cores above 0".into());
        }
        if self.max_memory == Some(0) {
            return Some("max_memory: above 0".into());
        }
        if self.port == Some(0) {
            return Some("port: 1 to 65535".into());
        }
        if let Some(d) = &self.distro
            && !valid_distro(d)
        {
            return Some("distro: the name `wsl -l` shows (letters, digits, `.`, `-`, `_`)".into());
        }
        None
    }
}

/// A WSL distribution's name as `wsl -l` shows it; passed to `wsl.exe -d`.
pub fn valid_distro(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 64
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// One thing the agent needs, and whether this machine has it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    /// What was found.
    pub detail: String,
    /// What to run when it is missing: the command alone, to be copied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Where `fix` runs, or what comes before it ("In `Ubuntu` as root").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_note: Option<String>,
    /// Missing, the agent still starts (the GPU, the firewall rule).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// What the switch did of the agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The switch is off, and no agent of the hub runs.
    #[default]
    Off,
    /// Not on this platform (macOS).
    Unsupported,
    /// Something the agent needs is missing (`checks`).
    Blocked,
    Starting,
    Running,
    /// Off, and other members' lanes still run here: the agent takes no new
    /// ones and ends with the last.
    Draining,
    /// It ended soon after its start; tried again after a minute.
    Failed,
    /// A node agent started by hand runs for this membership: the hub
    /// leaves it alone.
    Foreign,
}

/// The agent as the hub runs it, per pool.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Host {
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where requesters reach the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// Whether an admin granted this machine to run lanes; `None` when the
    /// coordinator was not asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<bool>,
    /// Other members' lanes that run here.
    pub guests: usize,
    /// Windows: what the distribution has.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// Windows: the distributions there are (Docker Desktop's left out).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub distros: Vec<String>,
    /// For a coding agent: how to set up what `checks` misses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The agent's last lines of output.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
}

/// What to do with a pool's agent, from its switch and what runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Nothing,
    Start {
        new_lanes: bool,
    },
    /// Started again: other caps, or new lanes no longer taken.
    Restart {
        new_lanes: bool,
    },
    Stop,
}

/// What runs now, for [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Running {
    pub new_lanes: bool,
    /// Its caps, distribution or port differ from the switch.
    pub stale: bool,
}

/// The step from what runs (`running`) towards what the switch says, with
/// `guests` other members' lanes here. `stray`: an agent this hub did not
/// start may run (an earlier hub's in WSL, its first look at the pool): it
/// is ended, or replaced by one that takes no new lanes while guests run.
/// `guests`: other members' lanes here, `None` when not known (the
/// coordinator could not be asked, and no count is known yet): an agent is
/// stopped only when there are none.
pub fn decide(enabled: bool, running: Option<Running>, guests: Option<usize>, stray: bool) -> Act {
    let none = guests == Some(0);
    match (enabled, running) {
        (true, None) => Act::Start { new_lanes: true },
        (true, Some(r)) if !r.new_lanes || r.stale => Act::Restart { new_lanes: true },
        (true, Some(_)) => Act::Nothing,
        (false, None) if stray && !none => Act::Restart { new_lanes: false },
        (false, None) if stray => Act::Stop,
        (false, None) => Act::Nothing,
        (false, Some(_)) if none => Act::Stop,
        (false, Some(r)) if r.new_lanes => Act::Restart { new_lanes: false },
        (false, Some(_)) => Act::Nothing,
    }
}

/// Other members' lanes here: what the coordinator says, else (it could not
/// be asked) the last count known, never none for want of an answer.
pub fn guests_or(asked: Option<usize>, last: Option<usize>) -> Option<usize> {
    asked.or(last)
}

/// The agent's arguments after `devstack` (Windows runs it as a process; on
/// Linux it runs inside the hub).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn agent_args(
    pool: &str,
    sw: &Switch,
    api: &str,
    membership: Option<&str>,
    new_lanes: bool,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "node".into(),
        "agent".into(),
        "--pool".into(),
        pool.into(),
        "--listen".into(),
        format!("0.0.0.0:{}", sw.port()),
        "--api".into(),
        api.into(),
    ];
    if let Some(m) = membership {
        a.extend(["--membership".into(), m.into()]);
    }
    if let Some(c) = sw.max_cpus {
        a.extend(["--max-cpus".into(), c.to_string()]);
    }
    if let Some(m) = sw.max_memory {
        a.extend([
            "--max-memory-gib".into(),
            format!("{:.2}", m as f64 / f64::from(1u32 << 30)),
        ]);
    }
    if !new_lanes {
        a.push("--no-new-lanes".into());
    }
    a
}

/// The address of this machine that reaches `coordinator`
/// (`https://host:port`): the one other members reach it at too, in the
/// same network. No packet is sent.
pub fn local_address(coordinator: &str) -> Option<std::net::IpAddr> {
    let rest = coordinator.split("://").nth(1).unwrap_or(coordinator);
    let hostport = rest.split('/').next()?;
    let target = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:443")
    };
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect(target).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// One agent the hub runs (none on macOS yet).
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Agent {
    new_lanes: bool,
    /// The switch it was started with.
    switch: Switch,
    started: Instant,
    output: std::sync::Arc<Mutex<Vec<String>>>,
    run: Run,
}

enum Run {
    #[cfg(target_os = "linux")]
    InProcess {
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        thread: std::thread::JoinHandle<()>,
    },
    #[cfg(windows)]
    Wsl {
        child: std::process::Child,
        distro: String,
    },
    /// Never made: keeps the enum inhabited where neither runs.
    #[allow(dead_code)]
    None,
}

impl Agent {
    fn ended(&mut self) -> bool {
        match &mut self.run {
            #[cfg(target_os = "linux")]
            Run::InProcess { thread, .. } => thread.is_finished(),
            #[cfg(windows)]
            Run::Wsl { child, .. } => !matches!(child.try_wait(), Ok(None)),
            Run::None => true,
        }
    }

    fn stop(self, pool: &str) {
        match self.run {
            #[cfg(target_os = "linux")]
            Run::InProcess { stop, thread } => {
                if let Some(s) = stop {
                    let _ = s.send(());
                }
                let _ = thread.join();
            }
            #[cfg(windows)]
            Run::Wsl { mut child, distro } => {
                wsl::kill_agent(&distro, pool);
                let _ = child.kill();
                let _ = child.wait();
                // The key does not stay where no agent needs it; a restart
                // copies it again.
                wsl::remove_membership(&distro, pool);
            }
            Run::None => {}
        }
        let _ = pool;
    }
}

#[derive(Default)]
struct Pools {
    hosts: BTreeMap<String, Host>,
    agents: BTreeMap<String, Agent>,
    /// When an agent of the pool last failed: not started again before
    /// [`BACKOFF`].
    failed: BTreeMap<String, Instant>,
    /// Pools whose agent is being replaced: it counts as running, so the
    /// hub does not report the machine meanwhile (#170).
    replacing: std::collections::BTreeSet<String>,
    /// Pools whose agent was acted on once (started, stopped or replaced):
    /// before, an earlier hub's agent may still run.
    seen: std::collections::BTreeSet<String>,
    /// The guests last counted per pool: known once the coordinator answered.
    guests: BTreeMap<String, usize>,
    /// When a blocked switch's checks last ran ([`RECHECK`]).
    checked: BTreeMap<String, Instant>,
}

static POOLS: OnceLock<Mutex<Pools>> = OnceLock::new();

/// Start looking after the switches, once per hub.
pub fn start(state: PathBuf) {
    if POOLS.set(Mutex::new(Pools::default())).is_err() {
        return;
    }
    std::thread::spawn(move || {
        loop {
            for (pool, dir) in devstack_node::member::pools(&state) {
                round(&pool, &dir, false);
            }
            std::thread::sleep(EVERY);
        }
    });
}

/// Act on a changed switch now, not at the next round; its checks run again.
pub fn poke(pool: &str, dir: &Path) {
    let (p, d) = (pool.to_string(), dir.to_path_buf());
    std::thread::spawn(move || round(&p, &d, true));
}

/// The pool's agent as last seen.
pub fn host(pool: &str) -> Host {
    POOLS
        .get()
        .and_then(|p| p.lock().unwrap().hosts.get(pool).cloned())
        .unwrap_or_default()
}

/// Whether an agent of the hub runs for the pool: the hub does not report
/// the machine then (#170).
pub fn runs(pool: &str) -> bool {
    POOLS.get().is_some_and(|p| {
        let p = p.lock().unwrap();
        p.agents.contains_key(pool) || p.replacing.contains(pool)
    })
}

fn set(pool: &str, f: impl FnOnce(&mut Host)) {
    if let Some(p) = POOLS.get() {
        f(p.lock().unwrap().hosts.entry(pool.to_string()).or_default());
    }
}

/// One look at a pool: the switch, what runs, and the step between them.
/// Serialised per hub: two rounds of one pool never act at once.
fn round(pool: &str, dir: &Path, poked: bool) {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap();
    let Some(pools) = POOLS.get() else { return };
    let mut sw = Switch::load(dir);
    // A distribution only by a name `wsl -l` could show, on every path (a
    // stop too), whoever wrote the file.
    if sw.distro.as_deref().is_some_and(|d| !valid_distro(d)) {
        sw.distro = None;
    }
    // Until the pool's agent was acted on: on Windows an earlier hub's agent
    // may still run in the distribution, unseen (its lock is in WSL).
    let stray = cfg!(windows) && sw.distro.is_some() && !pools.lock().unwrap().seen.contains(pool);
    let acted = || {
        pools.lock().unwrap().seen.insert(pool.to_string());
    };
    let local = devstack_node::member::Local::new(dir);
    let member = match local.membership() {
        Ok(Some(m)) if m.state == devstack_node::store::State::Approved => m,
        _ => {
            // No approved member: nothing may run.
            let agent = pools.lock().unwrap().agents.remove(pool);
            if let Some(a) = agent {
                a.stop(pool);
            }
            set(pool, |h| *h = Host::default());
            return;
        }
    };
    // Asked only where an agent runs or may: not every 5 s of every hub.
    let ours = sw.enabled || stray || pools.lock().unwrap().agents.contains_key(pool);
    let guests = if ours {
        let known = pools.lock().unwrap().guests.get(pool).copied();
        let g = guests_or(crate::share::guests(pool).map(|g| g.len()), known);
        if let Some(n) = g {
            pools.lock().unwrap().guests.insert(pool.to_string(), n);
        }
        g
    } else {
        Some(0)
    };
    let granted = if sw.enabled || pools.lock().unwrap().agents.contains_key(pool) {
        local
            .members()
            .ok()
            .and_then(|ms| ms.into_iter().find(|m| m.id == member.id))
            .map(|m| m.node)
    } else {
        None
    };
    set(pool, |h| {
        h.guests = guests.unwrap_or(h.guests);
        h.granted = granted;
    });

    if cfg!(target_os = "macos") {
        set(pool, |h| {
            h.state = State::Unsupported;
            h.detail = Some(MAC.into());
        });
        return;
    }

    // An agent of ours that ended: noted, and taken off.
    {
        let mut p = pools.lock().unwrap();
        if p.agents.get_mut(pool).is_some_and(Agent::ended) {
            let a = p.agents.remove(pool).expect("checked above");
            let soon = a.started.elapsed() < SETTLE;
            let out = a.output.lock().unwrap().clone();
            if soon {
                p.failed.insert(pool.to_string(), Instant::now());
            }
            let h = p.hosts.entry(pool.to_string()).or_default();
            h.output = out;
            if soon {
                h.state = State::Failed;
                h.detail = Some(
                    h.output
                        .iter()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .cloned()
                        .unwrap_or_else(|| ENDED.into()),
                );
            }
            drop(p);
            a.stop(pool);
        }
    }

    let running = {
        let p = pools.lock().unwrap();
        p.agents.get(pool).map(|a| Running {
            new_lanes: a.new_lanes,
            stale: Switch {
                enabled: true,
                ..a.switch.clone()
            } != Switch {
                enabled: true,
                ..sw.clone()
            },
        })
    };
    // An agent started by hand for this membership (on Linux; on Windows
    // the machine's own membership runs none).
    if running.is_none() && devstack_node::node::agent_runs(dir) {
        set(pool, |h| {
            h.state = State::Foreign;
            h.detail = Some(
                "A node agent started by hand runs for this machine: the switch leaves it alone."
                    .into(),
            );
        });
        return;
    }
    // An agent of ours runs: what an earlier hub left was dealt with.
    if running.is_some() {
        acted();
    }

    match decide(sw.enabled, running, guests, stray) {
        Act::Nothing => {
            let st = match running {
                Some(r) if r.new_lanes => State::Running,
                Some(_) => State::Draining,
                None => {
                    let h = host(pool);
                    if h.state == State::Failed && sw.enabled {
                        State::Failed
                    } else {
                        State::Off
                    }
                }
            };
            let output = pools
                .lock()
                .unwrap()
                .agents
                .get(pool)
                .map(|a| a.output.lock().unwrap().clone());
            set(pool, |h| {
                h.state = st;
                if st != State::Failed {
                    h.detail = None;
                }
                if let Some(o) = output {
                    h.output = o;
                }
                if st == State::Off {
                    h.api = None;
                    h.checks.clear();
                    h.prompt = None;
                }
            });
        }
        Act::Stop => {
            let agent = pools.lock().unwrap().agents.remove(pool);
            match agent {
                Some(a) => a.stop(pool),
                // An earlier hub's, and its copy of the membership.
                #[cfg(windows)]
                None => {
                    if let Some(d) = &sw.distro {
                        wsl::kill_agent(d, pool);
                        wsl::remove_membership(d, pool);
                    }
                }
                #[cfg(not(windows))]
                None => {}
            }
            acted();
            set(pool, |h| {
                h.state = State::Off;
                h.detail = None;
                h.api = None;
            });
        }
        Act::Start { new_lanes } | Act::Restart { new_lanes } => {
            let failed_at = pools.lock().unwrap().failed.get(pool).copied();
            if failed_at.is_some_and(|t| t.elapsed() < BACKOFF) && running.is_none() && new_lanes {
                return;
            }
            // Taking no new lanes needs nothing checked: it only narrows
            // what runs already, and a check that fails must not leave the
            // node taking lanes after it was switched off.
            if new_lanes {
                // Blocked, the checks run again once a minute or when asked.
                let recheck = pools.lock().unwrap().checked.get(pool).copied();
                if !poked
                    && host(pool).state == State::Blocked
                    && recheck.is_some_and(|t| t.elapsed() < RECHECK)
                {
                    return;
                }
                pools
                    .lock()
                    .unwrap()
                    .checked
                    .insert(pool.to_string(), Instant::now());
                if let Some(why) = sw.problem() {
                    set(pool, |h| {
                        h.state = State::Blocked;
                        h.detail = Some(why);
                    });
                    return;
                }
            }
            let ip = local_address(&member.coordinator);
            let api = match (ip, host(pool).api) {
                (Some(ip), _) => format!("https://{ip}:{}", sw.port()),
                // Draining keeps the address it had.
                (None, Some(api)) if !new_lanes => api,
                (None, _) => {
                    set(pool, |h| {
                        h.state = State::Blocked;
                        h.detail = Some(format!(
                            "No address of this machine reaches the coordinator at {}.",
                            member.coordinator
                        ));
                    });
                    return;
                }
            };
            if new_lanes && let Err(why) = ready(pool, &sw) {
                set(pool, |h| {
                    h.state = State::Blocked;
                    h.detail = Some(why);
                });
                return;
            }
            // Until the new one runs, the machine is the agents' to report.
            let old = {
                let mut p = pools.lock().unwrap();
                p.replacing.insert(pool.to_string());
                p.agents.remove(pool)
            };
            if let Some(a) = old {
                a.stop(pool);
            }
            set(pool, |h| {
                h.state = State::Starting;
                h.detail = None;
                h.api = Some(api.clone());
                h.output.clear();
            });
            let launched = launch(pool, dir, &sw, &api, new_lanes);
            // `launch` ends an earlier hub's agent first, whether it starts.
            acted();
            match launched {
                Ok(agent) => {
                    let mut p = pools.lock().unwrap();
                    p.failed.remove(pool);
                    p.replacing.remove(pool);
                    p.agents.insert(pool.to_string(), agent);
                    let h = p.hosts.entry(pool.to_string()).or_default();
                    h.state = if new_lanes {
                        State::Running
                    } else {
                        State::Draining
                    };
                }
                Err(e) => {
                    {
                        let mut p = pools.lock().unwrap();
                        p.failed.insert(pool.to_string(), Instant::now());
                        p.replacing.remove(pool);
                    }
                    set(pool, |h| {
                        h.state = State::Failed;
                        h.detail = Some(e);
                    });
                }
            }
        }
    }
}

/// What the agent would find with a switch, before it is on: the setup
/// dialog shows it (`POST /hub/pool/share/check`), and a start goes by it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Preflight {
    /// Every required check passes: the agent would start.
    pub ready: bool,
    /// Why it would not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// Windows: the distributions there are (Docker Desktop's left out).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub distros: Vec<String>,
    /// For a coding agent: how to set up what `checks` misses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl Preflight {
    fn not(problem: String) -> Self {
        Self {
            problem: Some(problem),
            ..Self::default()
        }
    }
}

/// Runs the checks (on Windows `wsl.exe` and PowerShell, seconds): not on an
/// async thread.
pub fn preflight(sw: &Switch) -> Preflight {
    if cfg!(target_os = "macos") {
        return Preflight::not(MAC.into());
    }
    if let Some(why) = sw.problem() {
        return Preflight::not(why);
    }
    needs(sw)
}

/// Whether what the agent needs is there. On Linux the agent's own start
/// says what is missing.
fn needs(sw: &Switch) -> Preflight {
    #[cfg(windows)]
    {
        let distros = wsl::distros();
        let Some(distro) = sw.distro.clone() else {
            return Preflight {
                prompt: Some(wsl::prompt(None, &[], sw.port())),
                distros,
                ..Preflight::not(
                    "Choose the WSL distribution the node agent runs in (one with Docker Engine of its own, not Docker Desktop's)."
                        .into(),
                )
            };
        };
        let checks = wsl::checks(&distro, &distros, sw.port());
        let missing = checks.iter().any(|c| !c.ok && !c.optional);
        let prompt = checks
            .iter()
            .any(|c| !c.ok)
            .then(|| wsl::prompt(Some(&distro), &checks, sw.port()));
        Preflight {
            ready: !missing,
            problem: missing.then(|| {
                format!(
                    "The WSL distribution `{distro}` lacks what the node agent needs: see the checks."
                )
            }),
            checks,
            distros,
            prompt,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = sw;
        Preflight {
            ready: true,
            ..Preflight::default()
        }
    }
}

/// [`needs`], with the checks put on the pool's [`Host`].
fn ready(pool: &str, sw: &Switch) -> Result<(), String> {
    let p = needs(sw);
    set(pool, |h| {
        h.distros = p.distros;
        h.checks = p.checks;
        h.prompt = p.prompt;
    });
    p.problem.map_or(Ok(()), Err)
}

/// Last lines of output kept per agent.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const KEEP_LINES: usize = 40;

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn keep(out: &Mutex<Vec<String>>, line: String) {
    let mut o = out.lock().unwrap();
    o.push(line);
    let n = o.len();
    if n > KEEP_LINES {
        o.drain(..n - KEEP_LINES);
    }
}

fn launch(
    pool: &str,
    dir: &Path,
    sw: &Switch,
    api: &str,
    new_lanes: bool,
) -> Result<Agent, String> {
    let output = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    #[cfg(target_os = "linux")]
    {
        let _ = &output;
        let config = devstack_node::node::NodeConfig {
            dir: dir.to_path_buf(),
            api: api.to_string(),
            share: devstack_node::share::Settings {
                enabled: new_lanes,
                max_cpus: sw.max_cpus,
                max_memory: sw.max_memory,
            },
            dedicated: false,
            free: None,
            on_update: None,
        };
        let listen: std::net::SocketAddr = ([0, 0, 0, 0], sw.port()).into();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let out = output.clone();
        let thread = std::thread::Builder::new()
            .name(format!("node-agent-{pool}"))
            .spawn(move || {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => return keep(&out, e.to_string()),
                };
                let r = rt.block_on(async move {
                    let listener = tokio::net::TcpListener::bind(listen)
                        .await
                        .map_err(|e| format!("listen on {listen}: {e}"))?;
                    devstack_node::node::serve(config, listener, async {
                        let _ = rx.await;
                    })
                    .await
                    .map_err(|e| e.to_string())
                });
                if let Err(e) = r {
                    keep(&out, e);
                }
                // A blocking task of a lane's start (a build) does not hold
                // the stop: the hub waits a few seconds, not minutes.
                rt.shutdown_timeout(STOP_WAIT);
            })
            .map_err(|e| e.to_string())?;
        Ok(Agent {
            new_lanes,
            switch: sw.clone(),
            started: Instant::now(),
            output,
            run: Run::InProcess {
                stop: Some(tx),
                thread,
            },
        })
    }
    #[cfg(windows)]
    {
        let distro = sw.distro.clone().ok_or("no WSL distribution chosen")?;
        let target = format!("{WSL_DIR}/{pool}");
        // One from an earlier hub may still run there: ended first, also
        // when the copy then fails.
        wsl::kill_agent(&distro, pool);
        wsl::copy_membership(&distro, dir, &target)?;
        let args = agent_args(pool, sw, api, Some(&target), new_lanes);
        let child = wsl::spawn_agent(&distro, &args, output.clone())?;
        Ok(Agent {
            new_lanes,
            switch: sw.clone(),
            started: Instant::now(),
            output,
            run: Run::Wsl { child, distro },
        })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (pool, dir, sw, api, new_lanes, output);
        Err("not on this platform".into())
    }
}

/// The distribution's side on Windows: listed, checked, given the
/// membership, and running the agent, all through `wsl.exe`.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod wsl {
    use super::{Check, FIREWALL_RULE, WSL_DIR};
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// `wsl.exe` lists in UTF-16 (little endian) where it is not told
    /// otherwise.
    pub fn decode(out: &[u8]) -> String {
        if out.len() >= 2
            && out.len().is_multiple_of(2)
            && out.iter().skip(1).step_by(2).all(|b| *b == 0)
        {
            let units: Vec<u16> = out
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            String::from_utf8_lossy(out).into_owned()
        }
    }

    /// The names `wsl -l -q` prints, Docker Desktop's left out.
    pub fn parse_list(text: &str) -> Vec<String> {
        text.lines()
            .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
            .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
            .map(str::to_string)
            .collect()
    }

    /// `wsl.exe`, asked for UTF-8: what it prints itself is UTF-16 otherwise,
    /// and a reader that splits its output into lines at `\n` cuts each
    /// UTF-16 line one byte off, so anything beyond ASCII (a localised
    /// message) read back as U+FFFD (code review before 0.51.0, finding 6).
    /// [`decode`] still reads UTF-16 from a `wsl.exe` that ignores it.
    pub fn command() -> Command {
        let mut c = Command::new("wsl.exe");
        c.env("WSL_UTF8", "1");
        devstack_desktop::no_window(&mut c);
        c
    }

    pub fn distros() -> Vec<String> {
        command()
            .args(["-l", "-q"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| parse_list(&decode(&o.stdout)))
            .unwrap_or_default()
    }

    /// `script` in `distro` as root, with `~/.local/bin` (where
    /// `install.sh` puts devstack) and WSL's GPU tools on `PATH`. With
    /// `--exec`, not `--`: that hands the command to the distribution's
    /// shell first, which expands the script before `sh` sees it (#597).
    pub fn sh(distro: &str, script: &str) -> Command {
        let mut c = command();
        c.args(["-d", distro, "-u", "root", "--exec", "sh", "-c"]);
        c.arg(format!(
            "PATH=\"$HOME/.local/bin:/usr/local/bin:/usr/lib/wsl/lib:$PATH\"; export PATH; {script}"
        ));
        c
    }

    /// What the checks read in the distribution, one `key=value` per line.
    const PROBE: &str = r#"echo "mode=$(wslinfo --networking-mode 2>/dev/null || echo unknown)"
echo "docker=$(docker info --format '{{.OperatingSystem}}' 2>/dev/null)"
echo "nvidia_runtime=$(docker info --format '{{json .Runtimes}}' 2>/dev/null | grep -c nvidia)"
echo "gpus=$(nvidia-smi -L 2>/dev/null | grep -c '^GPU')"
echo "devstack=$(devstack --version 2>/dev/null)"
echo "memory=$(awk '/MemTotal/ {print $2 * 1024}' /proc/meminfo)""#;

    /// The probe's lines as a map.
    /// Each value is cut to a short line of plain characters: it is what
    /// programs in the distribution printed, and it reaches the prompt a
    /// coding agent is handed (security review before 0.51.0, finding 2).
    pub fn parse_probe(text: &str) -> std::collections::BTreeMap<String, String> {
        let plain = |v: &str| -> String {
            v.chars()
                .filter(|c| c.is_ascii_alphanumeric() || " .-_:/()+,[]{}\"".contains(*c))
                .take(80)
                .collect::<String>()
                .trim()
                .to_string()
        };
        text.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), plain(v.trim())))
            .collect()
    }

    /// What `distro` has, as the agent needs it, from the probe's values and
    /// whether the firewall rule exists (`None`: not known).
    pub fn judge(
        distro: &str,
        exists: bool,
        probe: &std::collections::BTreeMap<String, String>,
        firewall: Option<bool>,
        version: &str,
        port: u16,
    ) -> Vec<Check> {
        let get = |k: &str| probe.get(k).map(String::as_str).unwrap_or("");
        let mut c = vec![Check {
            name: "distribution".into(),
            ok: exists,
            detail: if exists {
                format!("`{distro}` is there")
            } else {
                format!("no WSL distribution `{distro}`")
            },
            fix: (!exists).then(|| format!("wsl --install -d {distro}")),
            fix_note: (!exists).then(|| "In PowerShell".into()),
            optional: false,
        }];
        if !exists {
            return c;
        }
        let mode = get("mode");
        c.push(Check {
            name: "networking".into(),
            ok: mode == "mirrored",
            detail: format!("networking mode: {}", if mode.is_empty() { "unknown" } else { mode }),
            fix: (mode != "mirrored").then(|| "wsl --shutdown".into()),
            fix_note: (mode != "mirrored").then(|| {
                "In %UserProfile%\\.wslconfig under [wsl2] set networkingMode=mirrored, then in PowerShell (ends Docker Desktop too: start it again)".into()
            }),
            optional: false,
        });
        let docker = get("docker");
        let own = !docker.is_empty() && !docker.contains("Docker Desktop");
        c.push(Check {
            name: "docker".into(),
            ok: own,
            detail: if docker.is_empty() {
                "no Docker engine answers in the distribution".into()
            } else if own {
                format!("Docker Engine: {docker}")
            } else {
                "`docker` talks to Docker Desktop".into()
            },
            fix: (!own).then(|| "curl -fsSL https://get.docker.com | sh".into()),
            fix_note: (!own).then(|| {
                format!(
                    "Turn Docker Desktop's WSL integration off for `{distro}`, then in `{distro}` as root"
                )
            }),
            optional: false,
        });
        let dv = get("devstack");
        let same = dv.split_whitespace().nth(1) == Some(version);
        c.push(Check {
            name: "devstack".into(),
            ok: same,
            detail: if dv.is_empty() {
                "devstack is not installed in the distribution".into()
            } else {
                format!("{dv} (this machine: {version})")
            },
            // The script of the release asked for, not the newest one's:
            // what runs as root is the one that release was checked with.
            fix: (!same).then(|| {
                format!(
                    "curl -fsSL https://github.com/nuetzliches/devstack/releases/download/v{version}/install.sh | sh -s -- --version {version}"
                )
            }),
            fix_note: (!same).then(|| format!("In `{distro}` as root")),
            optional: false,
        });
        let gpus: u32 = get("gpus").parse().unwrap_or(0);
        let runtime = get("nvidia_runtime").parse::<u32>().unwrap_or(0) > 0;
        c.push(Check {
            name: "gpu".into(),
            ok: gpus == 0 || runtime,
            detail: match (gpus, runtime) {
                (0, _) => "no NVIDIA GPU visible (the node offers none)".into(),
                (n, true) => format!("{n} NVIDIA GPU(s), the container toolkit is set up"),
                (n, false) => format!("{n} NVIDIA GPU(s), but Docker has no `nvidia` runtime"),
            },
            fix: (gpus > 0 && !runtime).then(|| {
                "nvidia-ctk runtime configure --runtime=docker && systemctl restart docker".into()
            }),
            fix_note: (gpus > 0 && !runtime).then(|| {
                format!("Install the NVIDIA Container Toolkit in `{distro}`, then in it as root")
            }),
            optional: true,
        });
        c.push(Check {
            name: "firewall".into(),
            ok: firewall == Some(true),
            detail: match firewall {
                Some(true) => format!("Hyper-V firewall rule `{FIREWALL_RULE}` is there"),
                Some(false) => format!("no Hyper-V firewall rule `{FIREWALL_RULE}`: other members may not reach port {port}"),
                None => "the Hyper-V firewall rule could not be read".into(),
            },
            fix_note: (firewall != Some(true)).then(|| "In an elevated PowerShell".into()),
            fix: (firewall != Some(true)).then(|| {
                format!(
                    "New-NetFirewallHyperVRule -Name {FIREWALL_RULE} -DisplayName \"devstack node agent\" -Direction Inbound -VMCreatorId '{{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}}' -Protocol TCP -LocalPorts {port}"
                )
            }),
            optional: true,
        });
        c
    }

    /// Whether the Hyper-V firewall rule exists; `None` when it cannot be
    /// read (an older Windows, or PowerShell refused).
    fn firewall() -> Option<bool> {
        let mut c = Command::new("powershell.exe");
        devstack_desktop::no_window(&mut c);
        let out = c
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "if (Get-NetFirewallHyperVRule -Name {FIREWALL_RULE} -ErrorAction SilentlyContinue) {{ 'yes' }} else {{ 'no' }}"
                ),
            ])
            .output()
            .ok()?;
        match String::from_utf8_lossy(&out.stdout).trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        }
    }

    pub fn checks(distro: &str, distros: &[String], port: u16) -> Vec<Check> {
        let exists = distros.iter().any(|d| d.eq_ignore_ascii_case(distro));
        let probe = if exists {
            sh(distro, PROBE)
                .stdin(Stdio::null())
                .output()
                .map(|o| parse_probe(&String::from_utf8_lossy(&o.stdout)))
                .unwrap_or_default()
        } else {
            Default::default()
        };
        let version = devstack_node::version::Release::this().version;
        judge(distro, exists, &probe, firewall(), &version, port)
    }

    /// For a coding agent on this machine: set up what the checks miss.
    pub fn prompt(distro: Option<&str>, checks: &[Check], port: u16) -> String {
        let mut p = String::from(
            "Set up a WSL2 distribution on this Windows machine so that devstack can run its node agent there, for other members of the devstack pool to run lanes on this machine. Ask before each step that changes the machine, and say what it changes.\n\n",
        );
        match distro {
            None => p.push_str(
                "1. Pick or create a distribution of its own, not one Docker Desktop's WSL integration uses: `wsl --install -d Ubuntu-24.04`, initialised as root. Then enter its name in the dashboard (Pool, This machine).\n",
            ),
            Some(d) => {
                p.push_str(&format!(
                    "The distribution is `{d}`; run commands in it as root (`wsl -d {d} -u root`).\n"
                ));
                let mut n = 0;
                for c in checks.iter().filter(|c| !c.ok) {
                    n += 1;
                    p.push_str(&format!("{n}. {}: {}", c.name, c.detail));
                    match (&c.fix_note, &c.fix) {
                        (Some(w), Some(f)) => p.push_str(&format!(". {w}: `{f}`")),
                        (None, Some(f)) => p.push_str(&format!(". `{f}`")),
                        _ => {}
                    }
                    p.push('\n');
                }
            }
        }
        p.push_str(&format!(
            "\nThe node agent listens on port {port}; the dashboard starts it once every required check passes. Do not start the agent by hand. Steps that need an elevated shell (the firewall rule) are for the user to run: show them the command."
        ));
        p
    }

    /// The membership's files (not its folders: the Windows side's state)
    /// into `target` in the distribution, readable by root only.
    pub fn copy_membership(distro: &str, dir: &Path, target: &str) -> Result<(), String> {
        let files: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter(|e| e.file_name() != "node-host.json")
            .collect();
        let mkdir = sh(
            distro,
            &format!(
                "umask 077 && mkdir -p {t} && chmod 700 {w} {t}",
                t = quote(target),
                w = quote(WSL_DIR)
            ),
        )
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("wsl.exe: {e}"))?;
        if !mkdir.status.success() {
            return Err(format!(
                "cannot make {target} in `{distro}`: {}",
                text(&mkdir.stderr)
            ));
        }
        for f in files {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            {
                continue;
            }
            let bytes = std::fs::read(f.path()).map_err(|e| format!("{name}: {e}"))?;
            let mut child = sh(
                distro,
                &format!("umask 077 && cat > {}", quote(&format!("{target}/{name}"))),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("wsl.exe: {e}"))?;
            {
                use std::io::Write;
                let mut stdin = child.stdin.take().ok_or("wsl.exe: no stdin")?;
                stdin
                    .write_all(&bytes)
                    .map_err(|e| format!("{name} into `{distro}`: {e}"))?;
            }
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!("{name} into `{distro}`: {}", text(&out.stderr)));
            }
        }
        Ok(())
    }

    /// Ends the pool's agent in the distribution, one an earlier hub left
    /// running included.
    pub fn kill_agent(distro: &str, pool: &str) {
        let _ = sh(
            distro,
            &format!("pkill -TERM -f -- '--membership {WSL_DIR}/{pool}( |$)' ; true"),
        )
        .stdin(Stdio::null())
        .output();
    }

    /// Removes the membership's copy from the distribution once its agent is
    /// gone (the switch off, or another distribution chosen).
    pub fn remove_membership(distro: &str, pool: &str) {
        let _ = sh(distro, &remove_script(&format!("{WSL_DIR}/{pool}")))
            .stdin(Stdio::null())
            .output();
    }

    /// What [`remove_membership`] runs in the folder `target`: the files
    /// [`copy_membership`] put there, not the agent's state in `node/`
    /// (leases, kept slots, its secret), which outlives a restart.
    pub fn remove_script(target: &str) -> String {
        format!(
            "find {} -maxdepth 1 -type f -exec rm -f -- {{}} + 2>/dev/null; true",
            quote(target)
        )
    }

    /// `s` as one word for `sh`, whatever it holds.
    pub fn quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', r"'\''"))
    }

    /// A line `wsl.exe` printed itself (UTF-16) or the agent printed (UTF-8),
    /// readable either way.
    pub fn text(bytes: &[u8]) -> String {
        decode(bytes).replace('\0', "").trim_end().to_string()
    }

    pub fn spawn_agent(
        distro: &str,
        args: &[String],
        output: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Result<std::process::Child, String> {
        let quoted: Vec<String> = args.iter().map(|a| quote(a)).collect();
        let mut child = sh(distro, &format!("exec devstack {}", quoted.join(" ")))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("wsl.exe: {e}"))?;
        for pipe in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let out = output.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut r = std::io::BufReader::new(pipe);
                let mut line = Vec::new();
                while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                    super::keep(&out, text(&line));
                    line.clear();
                }
            });
        }
        Ok(child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_and_what_runs_give_the_step() {
        let run = |new_lanes, stale| Some(Running { new_lanes, stale });
        let n = Some;
        assert_eq!(
            decide(true, None, n(0), false),
            Act::Start { new_lanes: true }
        );
        assert_eq!(decide(true, run(true, false), n(3), false), Act::Nothing);
        assert_eq!(
            decide(true, run(true, true), n(0), false),
            Act::Restart { new_lanes: true }
        );
        assert_eq!(
            decide(true, run(false, false), n(1), false),
            Act::Restart { new_lanes: true }
        );
        assert_eq!(decide(false, None, n(2), false), Act::Nothing);
        // Off with guests: no new lanes, the running ones go on.
        assert_eq!(
            decide(false, run(true, false), n(2), false),
            Act::Restart { new_lanes: false }
        );
        assert_eq!(decide(false, run(false, false), n(2), false), Act::Nothing);
        // The last one ended: the agent goes.
        assert_eq!(decide(false, run(false, false), n(0), false), Act::Stop);
        assert_eq!(decide(false, run(true, false), n(0), false), Act::Stop);
        // Not known: it may have guests, it stays.
        assert_eq!(decide(false, run(false, false), None, false), Act::Nothing);
        assert_eq!(
            decide(false, run(true, false), None, false),
            Act::Restart { new_lanes: false }
        );
    }

    #[test]
    fn an_earlier_hubs_agent_is_ended_or_kept_for_its_guests() {
        // Off, its lanes still running: one that takes no new lanes.
        assert_eq!(
            decide(false, None, Some(2), true),
            Act::Restart { new_lanes: false }
        );
        // Off, guests not known (a fresh hub, the coordinator not reached):
        // kept as if it had some (code review before 0.51.0, finding 4).
        assert_eq!(
            decide(false, None, None, true),
            Act::Restart { new_lanes: false }
        );
        // Off, no guests: it goes.
        assert_eq!(decide(false, None, Some(0), true), Act::Stop);
        // On: a new one replaces it.
        assert_eq!(
            decide(true, None, Some(0), true),
            Act::Start { new_lanes: true }
        );
    }

    #[test]
    fn guests_not_known_are_the_last_count_not_none() {
        assert_eq!(guests_or(None, Some(2)), Some(2));
        assert_eq!(guests_or(Some(0), Some(2)), Some(0));
        // Not asked yet and no answer: not known, not none.
        assert_eq!(guests_or(None, None), None);
    }

    /// #597: `wsl.exe … -- cmd` hands `cmd` to the distribution's default
    /// shell first, which expands `$HOME`, `$PATH` and every `$(…)` of the
    /// script before `sh` runs it: the probe then looked for devstack on
    /// the default `PATH`, without `~/.local/bin`. `--exec` runs `sh` with
    /// the script as one argument, as written.
    #[test]
    fn a_script_reaches_sh_in_the_distribution_as_written() {
        let script = "echo \"devstack=$(devstack --version)\"";
        let c = wsl::sh("Ubuntu-24.04", script);
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let exec = args.iter().position(|a| a == "--exec");
        assert!(exec.is_some(), "{args:?}");
        assert!(!args.iter().any(|a| a == "--"), "{args:?}");
        let rest = &args[exec.unwrap() + 1..];
        assert_eq!(rest[..2], ["sh", "-c"], "{args:?}");
        assert_eq!(rest.len(), 3, "the script is one argument: {args:?}");
        assert!(rest[2].ends_with(script) && rest[2].contains("$HOME/.local/bin"));
    }

    #[test]
    fn words_for_sh_and_lines_of_wsl_exe() {
        assert_eq!(wsl::quote("a b"), "'a b'");
        assert_eq!(wsl::quote("it's"), r"'it'\''s'");
        let utf16: Vec<u8> = "There is no distribution\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(wsl::text(&utf16), "There is no distribution");
        assert_eq!(
            wsl::text(b"listening on 0.0.0.0:4261\n"),
            "listening on 0.0.0.0:4261"
        );
        // Asked for UTF-8, a localised line read line by line keeps its
        // letters (as UTF-16, the second line began with the first's `\0`).
        let utf8 = "Die Verteilung wurde gefunden.\nfür den Knoten\n".as_bytes();
        let lines: Vec<String> = utf8
            .split_inclusive(|b| *b == b'\n')
            .map(wsl::text)
            .collect();
        assert_eq!(lines[1], "für den Knoten");
        assert!(
            wsl::command()
                .get_envs()
                .any(|(k, v)| k == "WSL_UTF8" && v == Some(std::ffi::OsStr::new("1")))
        );
    }

    /// The agent keeps its state in `<membership>/node/` (leases, kept
    /// slots, the node's secret): a stop takes away the membership's copy,
    /// never that, or a restart of the agent loses the lanes of its guests
    /// (code review before 0.51.0, finding 1). Run where `sh` is (the
    /// distribution's side is a POSIX shell).
    #[test]
    fn removing_the_membership_keeps_the_agents_state() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("default");
        std::fs::create_dir_all(target.join("node/slots/a")).unwrap();
        for f in ["member.key", "member.pem", "ca.pem", "membership.json"] {
            std::fs::write(target.join(f), "x").unwrap();
        }
        std::fs::write(target.join("node/leases.json"), "{}").unwrap();
        let script = wsl::remove_script(&target.to_string_lossy().replace('\\', "/"));
        let Ok(out) = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
        else {
            eprintln!("no `sh` here: not run");
            return;
        };
        assert!(out.status.success(), "{out:?}");
        assert!(!target.join("member.key").exists(), "the key goes");
        assert!(!target.join("membership.json").exists());
        assert!(target.join("node/leases.json").is_file(), "the state stays");
        assert!(target.join("node/slots/a").is_dir());
    }

    #[test]
    fn the_agent_is_told_the_switch() {
        let sw = Switch {
            enabled: true,
            max_cpus: Some(8.0),
            max_memory: Some(8 << 30),
            distro: Some("Ubuntu-24.04".into()),
            port: None,
        };
        let a = agent_args(
            "default",
            &sw,
            "https://10.0.0.7:4261",
            Some("/var/lib/devstack-node/default"),
            false,
        );
        let s = a.join(" ");
        assert_eq!(
            s,
            "node agent --pool default --listen 0.0.0.0:4261 --api https://10.0.0.7:4261 --membership /var/lib/devstack-node/default --max-cpus 8 --max-memory-gib 8.00 --no-new-lanes"
        );
        assert!(
            !agent_args("default", &Switch::default(), "https://x:4261", None, true)
                .contains(&"--no-new-lanes".to_string())
        );
    }

    #[test]
    fn an_impossible_switch_is_refused() {
        let ok = Switch {
            enabled: true,
            distro: Some("Ubuntu-24.04".into()),
            ..Switch::default()
        };
        assert_eq!(ok.problem(), None);
        for bad in [
            Switch {
                max_cpus: Some(0.0),
                ..ok.clone()
            },
            Switch {
                max_memory: Some(0),
                ..ok.clone()
            },
            Switch {
                port: Some(0),
                ..ok.clone()
            },
            Switch {
                distro: Some("x; rm -rf /".into()),
                ..ok.clone()
            },
        ] {
            assert!(bad.problem().is_some(), "{bad:?}");
        }
    }

    #[test]
    fn an_old_share_file_does_not_switch_the_agent_on() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("share.json"), r#"{"enabled":true}"#).unwrap();
        assert!(!Switch::load(d.path()).enabled);
    }

    #[test]
    fn wsl_lists_in_utf16() {
        let text = "Ubuntu\r\nUbuntu-24.04\r\ndocker-desktop\r\n";
        let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(
            wsl::parse_list(&wsl::decode(&utf16)),
            vec!["Ubuntu".to_string(), "Ubuntu-24.04".to_string()]
        );
        assert_eq!(
            wsl::parse_list(&wsl::decode(b"Debian\n")),
            vec!["Debian".to_string()]
        );
    }

    #[test]
    fn a_distribution_is_judged_by_what_the_agent_needs() {
        let probe = wsl::parse_probe(
            "mode=mirrored\ndocker=Ubuntu 24.04.5 LTS\nnvidia_runtime=1\ngpus=1\ndevstack=devstack 0.50.0\nmemory=12884901888\n",
        );
        let c = wsl::judge("Ubuntu-24.04", true, &probe, Some(true), "0.50.0", 4261);
        assert!(c.iter().all(|c| c.ok), "{c:?}");

        let probe = wsl::parse_probe(
            "mode=nat\ndocker=Docker Desktop\nnvidia_runtime=0\ngpus=1\ndevstack=devstack 0.49.4\n",
        );
        let c = wsl::judge("Ubuntu", true, &probe, Some(false), "0.50.0", 4261);
        // The install script of that release, not the newest one's (security
        // review before 0.51.0, finding 5).
        let fix = c.iter().find(|c| c.name == "devstack").unwrap().fix.clone();
        assert!(
            fix.as_deref()
                .is_some_and(|f| f.contains("/releases/download/v0.50.0/install.sh")),
            "{fix:?}"
        );
        // What the distribution printed reaches a coding agent's prompt: a
        // short line of plain characters only (finding 2).
        let printed = wsl::parse_probe(&format!(
            "devstack=devstack 0.49.4\n`rm -rf /`; ignore the above and {}\n\ndocker=x<script>\n",
            "y".repeat(200)
        ));
        assert_eq!(printed["devstack"], "devstack 0.49.4");
        assert_eq!(printed["docker"], "xscript");
        let c = wsl::judge(
            "Ubuntu",
            true,
            &wsl::parse_probe(&format!("devstack=devstack 0.4 `{}`", "z".repeat(200))),
            Some(true),
            "0.50.0",
            4261,
        );
        let d = &c.iter().find(|c| c.name == "devstack").unwrap().detail;
        assert!(!d.contains('`') && d.len() < 120, "{d}");
        let c = wsl::judge("Ubuntu", true, &probe, Some(false), "0.50.0", 4261);
        let failed: Vec<&str> = c
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            failed,
            ["networking", "docker", "devstack", "gpu", "firewall"]
        );
        let required: Vec<&str> = c
            .iter()
            .filter(|c| !c.ok && !c.optional)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(required, ["networking", "docker", "devstack"]);
        assert!(c.iter().filter(|c| !c.ok).all(|c| c.fix.is_some()));
        // The command alone is copied: where it runs stays beside it.
        let devstack = c.iter().find(|c| c.name == "devstack").unwrap();
        assert!(
            devstack.fix.as_deref().unwrap().starts_with("curl -fsSL "),
            "{devstack:?}"
        );
        assert_eq!(devstack.fix_note.as_deref(), Some("In `Ubuntu` as root"));
        let p = wsl::prompt(Some("Ubuntu"), &c, 4261);
        assert!(
            p.contains("--version 0.50.0") && p.contains("networkingMode=mirrored"),
            "{p}"
        );

        let c = wsl::judge("Gone", false, &Default::default(), None, "0.50.0", 4261);
        assert_eq!(c.len(), 1);
        assert!(!c[0].ok);
    }

    #[test]
    fn the_address_toward_the_coordinator_is_a_local_one() {
        let ip = local_address("https://127.0.0.1:4260").unwrap();
        assert!(ip.is_loopback());
        assert_eq!(local_address("not a url"), None);
    }
}
