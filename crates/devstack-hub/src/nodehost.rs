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
/// Where the hub on Windows puts the membership's copy in the distribution.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WSL_DIR: &str = "/var/lib/devstack-node";
/// The Hyper-V firewall rule the setup names (`New-NetFirewallHyperVRule`).
#[cfg_attr(not(windows), allow(dead_code))]
pub const FIREWALL_RULE: &str = "devstack-node";

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
    /// What to run when it is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
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
/// `guests` other members' lanes here.
pub fn decide(enabled: bool, running: Option<Running>, guests: usize) -> Act {
    match (enabled, running) {
        (true, None) => Act::Start { new_lanes: true },
        (true, Some(r)) if !r.new_lanes || r.stale => Act::Restart { new_lanes: true },
        (true, Some(_)) => Act::Nothing,
        (false, None) => Act::Nothing,
        (false, Some(_)) if guests == 0 => Act::Stop,
        (false, Some(r)) if r.new_lanes => Act::Restart { new_lanes: false },
        (false, Some(_)) => Act::Nothing,
    }
}

/// The agent's arguments after `devstack`.
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
                round(&pool, &dir);
            }
            std::thread::sleep(EVERY);
        }
    });
}

/// Act on a changed switch now, not at the next round.
pub fn poke(pool: &str, dir: &Path) {
    let (p, d) = (pool.to_string(), dir.to_path_buf());
    std::thread::spawn(move || round(&p, &d));
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
    POOLS
        .get()
        .is_some_and(|p| p.lock().unwrap().agents.contains_key(pool))
}

fn set(pool: &str, f: impl FnOnce(&mut Host)) {
    if let Some(p) = POOLS.get() {
        f(p.lock().unwrap().hosts.entry(pool.to_string()).or_default());
    }
}

/// One look at a pool: the switch, what runs, and the step between them.
/// Serialised per hub: two rounds of one pool never act at once.
fn round(pool: &str, dir: &Path) {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap();
    let Some(pools) = POOLS.get() else { return };
    let sw = Switch::load(dir);
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
    let guests = crate::share::guests(pool).map_or(0, |g| g.len());
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
        h.guests = guests;
        h.granted = granted;
    });

    if cfg!(target_os = "macos") {
        set(pool, |h| {
            h.state = State::Unsupported;
            h.detail = Some("A Mac cannot run other members' lanes yet.".into());
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
                        .unwrap_or_else(|| "the node agent ended".into()),
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

    match decide(sw.enabled, running, guests) {
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
            if let Some(a) = agent {
                a.stop(pool);
            }
            set(pool, |h| {
                h.state = State::Off;
                h.detail = None;
                h.api = None;
            });
        }
        Act::Start { new_lanes } | Act::Restart { new_lanes } => {
            let failed_at = pools.lock().unwrap().failed.get(pool).copied();
            if failed_at.is_some_and(|t| t.elapsed() < BACKOFF) && running.is_none() {
                return;
            }
            if let Some(why) = sw.problem() {
                set(pool, |h| {
                    h.state = State::Blocked;
                    h.detail = Some(why);
                });
                return;
            }
            let Some(ip) = local_address(&member.coordinator) else {
                set(pool, |h| {
                    h.state = State::Blocked;
                    h.detail = Some(format!(
                        "No address of this machine reaches the coordinator at {}.",
                        member.coordinator
                    ));
                });
                return;
            };
            let api = format!("https://{ip}:{}", sw.port());
            if let Err(why) = ready(pool, &sw) {
                set(pool, |h| {
                    h.state = State::Blocked;
                    h.detail = Some(why);
                });
                return;
            }
            let old = pools.lock().unwrap().agents.remove(pool);
            if let Some(a) = old {
                a.stop(pool);
            }
            set(pool, |h| {
                h.state = State::Starting;
                h.detail = None;
                h.api = Some(api.clone());
                h.output.clear();
            });
            match launch(pool, dir, &sw, &api, new_lanes) {
                Ok(agent) => {
                    let mut p = pools.lock().unwrap();
                    p.failed.remove(pool);
                    p.agents.insert(pool.to_string(), agent);
                    let h = p.hosts.entry(pool.to_string()).or_default();
                    h.state = if new_lanes {
                        State::Running
                    } else {
                        State::Draining
                    };
                }
                Err(e) => {
                    pools
                        .lock()
                        .unwrap()
                        .failed
                        .insert(pool.to_string(), Instant::now());
                    set(pool, |h| {
                        h.state = State::Failed;
                        h.detail = Some(e);
                    });
                }
            }
        }
    }
}

/// Whether what the agent needs is there; the checks go to the pool's
/// [`Host`]. On Linux the agent's own start says what is missing.
fn ready(pool: &str, sw: &Switch) -> Result<(), String> {
    #[cfg(windows)]
    {
        let distros = wsl::distros();
        set(pool, |h| h.distros = distros.clone());
        let Some(distro) = sw.distro.clone() else {
            set(pool, |h| {
                h.checks.clear();
                h.prompt = Some(wsl::prompt(None, &[], sw.port()));
            });
            return Err(
                "Choose the WSL distribution the node agent runs in (one with Docker Engine of its own, not Docker Desktop's)."
                    .into(),
            );
        };
        let checks = wsl::checks(&distro, &distros, sw.port());
        let missing = checks.iter().any(|c| !c.ok && !c.optional);
        let prompt = checks
            .iter()
            .any(|c| !c.ok)
            .then(|| wsl::prompt(Some(&distro), &checks, sw.port()));
        set(pool, |h| {
            h.checks = checks;
            h.prompt = prompt;
        });
        if missing {
            return Err(format!(
                "The WSL distribution `{distro}` lacks what the node agent needs: see the checks."
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (pool, sw);
        Ok(())
    }
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
    let output = std::sync::Arc::new(Mutex::new(Vec::new()));
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
        wsl::copy_membership(&distro, dir, &target)?;
        // One from an earlier hub may still run there.
        wsl::kill_agent(&distro, pool);
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

    fn wsl() -> Command {
        let mut c = Command::new("wsl.exe");
        devstack_desktop::no_window(&mut c);
        c
    }

    pub fn distros() -> Vec<String> {
        wsl()
            .args(["-l", "-q"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| parse_list(&decode(&o.stdout)))
            .unwrap_or_default()
    }

    /// `script` in `distro` as root, with `~/.local/bin` (where
    /// `install.sh` puts devstack) and WSL's GPU tools on `PATH`.
    fn sh(distro: &str, script: &str) -> Command {
        let mut c = wsl();
        c.args(["-d", distro, "-u", "root", "--", "sh", "-c"]);
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
    pub fn parse_probe(text: &str) -> std::collections::BTreeMap<String, String> {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
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
            fix: (mode != "mirrored").then(|| {
                "In %UserProfile%\\.wslconfig under [wsl2]: networkingMode=mirrored, then `wsl --shutdown` (ends Docker Desktop too: start it again)".into()
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
            fix: (!own).then(|| {
                format!(
                    "Turn Docker Desktop's WSL integration off for `{distro}`, then in it as root: curl -fsSL https://get.docker.com | sh"
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
            fix: (!same).then(|| {
                format!(
                    "In `{distro}` as root: curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh -s -- --version {version}"
                )
            }),
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
                "Install the NVIDIA Container Toolkit in the distribution, then: nvidia-ctk runtime configure --runtime=docker && systemctl restart docker".into()
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
            fix: (firewall != Some(true)).then(|| {
                format!(
                    "In an elevated PowerShell: New-NetFirewallHyperVRule -Name {FIREWALL_RULE} -DisplayName \"devstack node agent\" -Direction Inbound -VMCreatorId '{{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}}' -Protocol TCP -LocalPorts {port}"
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
                    if let Some(f) = &c.fix {
                        p.push_str(&format!(". {f}"));
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
            &format!("umask 077 && mkdir -p '{target}' && chmod 700 '{WSL_DIR}' '{target}'"),
        )
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("wsl.exe: {e}"))?;
        if !mkdir.status.success() {
            return Err(format!(
                "cannot make {target} in `{distro}`: {}",
                String::from_utf8_lossy(&mkdir.stderr).trim()
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
            let mut child = sh(distro, &format!("umask 077 && cat > '{target}/{name}'"))
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
                return Err(format!(
                    "{name} into `{distro}`: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
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

    pub fn spawn_agent(
        distro: &str,
        args: &[String],
        output: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Result<std::process::Child, String> {
        let quoted: Vec<String> = args.iter().map(|a| format!("'{a}'")).collect();
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
                for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
                    super::keep(&out, line);
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
        assert_eq!(decide(true, None, 0), Act::Start { new_lanes: true });
        assert_eq!(decide(true, run(true, false), 3), Act::Nothing);
        assert_eq!(
            decide(true, run(true, true), 0),
            Act::Restart { new_lanes: true }
        );
        assert_eq!(
            decide(true, run(false, false), 1),
            Act::Restart { new_lanes: true }
        );
        assert_eq!(decide(false, None, 2), Act::Nothing);
        // Off with guests: no new lanes, the running ones go on.
        assert_eq!(
            decide(false, run(true, false), 2),
            Act::Restart { new_lanes: false }
        );
        assert_eq!(decide(false, run(false, false), 2), Act::Nothing);
        // The last one ended: the agent goes.
        assert_eq!(decide(false, run(false, false), 0), Act::Stop);
        assert_eq!(decide(false, run(true, false), 0), Act::Stop);
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
