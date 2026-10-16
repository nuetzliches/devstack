//! A lane running on the node (ADR-0025 point 2, ADR-0028): the lane's
//! infrastructure as Compose projects of its own on the node's Docker
//! engine, and one lane container from the stack's lane image, in which the
//! node's own static Linux `devstack` runs the lane's daemon from the mirror.
//! The daemon there has no Docker: the agent describes the infrastructure to
//! it in a file (`DEVSTACK_EXTERNAL_INFRA`, spike M0).
//!
//! Everything here calls the Docker CLI and blocks.

use devstack_compose::{Docker, Group};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The port the lane's daemon listens on inside its container.
pub const DAEMON_PORT: u16 = 7600;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Compose(#[from] devstack_compose::ComposeError),
    #[error("{0}")]
    Config(String),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}")]
    Docker(String),
}

pub type Result<T> = std::result::Result<T, RunError>;

/// One lane on this node.
pub struct LaneRun {
    /// The lease id: names its Compose projects and its container.
    pub id: String,
    /// The lane's name, as its requester calls it.
    pub lane: String,
    /// `lanes/<id>` under the node's state.
    pub dir: PathBuf,
    /// The stack file, relative to the lane's root.
    pub stack_file: String,
    /// Where the requester has the lane's files (`DEVSTACK_DISPLAY_ROOT`).
    pub display_root: Option<String>,
    /// The lane's port offset on the requester's machine
    /// (`DEVSTACK_PORT_OFFSET`): it runs on those ports here too.
    pub port_offset: u16,
    /// The lane container's memory limit (ADR-0033), bytes.
    pub memory_limit: Option<u64>,
    /// Its CPU limit, cores.
    pub cpu_limit: Option<f64>,
    /// What its networks reach beyond the lane (ADR-0033 point 5).
    pub network: crate::policy::Network,
}

/// Where the lane's daemon is reached, and its token.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonAddr {
    /// `host:port` on the lane's network.
    pub addr: String,
    pub token: String,
    /// The ports its containers are published on at the requester's machine
    /// (the Compose files' host ports moved by the lane's offset): the
    /// requester forwards them like the services' (ADR-0029 point 4).
    pub infra_ports: Vec<u16>,
}

fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| RunError::Docker(format!("run docker: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(RunError::Docker(format!(
            "docker {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Groups in the order they start (`after`).
fn order(stack: &devstack_config::Stack) -> Vec<String> {
    fn visit(
        n: &str,
        stack: &devstack_config::Stack,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) {
        if !seen.insert(n.to_string()) {
            return;
        }
        for d in &stack.compose[n].after {
            if stack.compose.contains_key(d) {
                visit(d, stack, seen, out);
            }
        }
        out.push(n.to_string());
    }
    let (mut seen, mut out) = (BTreeSet::new(), Vec::new());
    for n in stack.compose.keys() {
        visit(n, stack, &mut seen, &mut out);
    }
    out
}

impl LaneRun {
    pub fn src(&self) -> PathBuf {
        self.dir.join("src")
    }

    fn container(&self) -> String {
        format!("devstack-lane-{}", self.id)
    }

    fn stack(&self) -> Result<devstack_config::Stack> {
        devstack_config::load(&self.src().join(&self.stack_file))
            .map_err(|e| RunError::Config(e.to_string()))
    }

    /// The lane's copy of a group: its own project, its own fixed-name
    /// networks, nothing shared (ADR-0025 point 3), the node overlay once it
    /// is written.
    fn group(&self, stack: &devstack_config::Stack, name: &str) -> Group {
        let cfg = &stack.compose[name];
        let mut g = Group::from_config(name, cfg, &self.src());
        g.project = format!("{}-{}", cfg.project, self.id);
        g.networks = cfg
            .networks
            .iter()
            .map(|n| format!("{n}-{}", self.id))
            .collect();
        let overlay = self.overlay(name);
        if overlay.is_file() {
            g.files.push(overlay);
        }
        g
    }

    fn overlay(&self, group: &str) -> PathBuf {
        self.dir.join("compose").join(format!("{group}.yml"))
    }

    /// The image the lane runs in: the stack's `[lanes] image`, pulled if the
    /// node lacks it, or one built from its `[lanes] dockerfile` and kept by
    /// the Dockerfile's content (ADR-0028).
    fn image(&self, stack: &devstack_config::Stack) -> Result<String> {
        if let Some(img) = &stack.lanes.image {
            if docker(&["image", "inspect", "--format", "{{.Id}}", img]).is_err() {
                docker(&["pull", img])?;
            }
            return Ok(img.clone());
        }
        let Some(df) = &stack.lanes.dockerfile else {
            return Err(RunError::Config(
                "the stack names no lane image: set `[lanes] image` or `[lanes] dockerfile` (ADR-0028)"
                    .into(),
            ));
        };
        let base = self
            .src()
            .join(&self.stack_file)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.src());
        let file = base.join(df);
        let text = std::fs::read(&file).map_err(|e| RunError::Io(file.clone(), e))?;
        let tag = format!("devstack-lane:{}", &blake3::hash(&text).to_hex()[..16]);
        if docker(&["image", "inspect", "--format", "{{.Id}}", &tag]).is_err() {
            docker(&[
                "build",
                "-q",
                "-t",
                &tag,
                "-f",
                &file.to_string_lossy(),
                &base.to_string_lossy(),
            ])?;
        }
        Ok(tag)
    }

    /// Start the lane: infrastructure, its description for the daemon, the
    /// lane container. `bin` is a static Linux `devstack` the container
    /// mounts; `agent`, the agent's own container when it runs in one (it
    /// joins the lane's network to reach the daemon).
    /// `phase` hears what it does now ("image", "infrastructure: app", ...).
    pub fn start(
        &self,
        bin: &Path,
        agent: Option<&str>,
        phase: &dyn Fn(&str),
    ) -> Result<DaemonAddr> {
        let stack = self.stack()?;
        phase("lane image: pulled or built once, then kept");
        let image = self.image(&stack)?;
        let dc = Docker::default();
        let mut infra = BTreeMap::new();
        let mut networks = Vec::new();
        let mut infra_ports = Vec::new();
        for name in order(&stack) {
            phase(&format!("infrastructure: {name}"));
            let overlay = self.overlay(&name);
            let _ = std::fs::remove_file(&overlay);
            let g = self.group(&stack, &name);
            let model = dc.config(&g)?;
            let isolated = self.network == crate::policy::Network::None;
            let text = devstack_compose::node_overlay(&model, &g.project, &self.id, isolated)
                .map_err(|e| RunError::Config(format!("compose group `{name}`: {e}")))?;
            std::fs::create_dir_all(overlay.parent().expect("has a parent"))
                .map_err(|e| RunError::Io(overlay.clone(), e))?;
            std::fs::write(&overlay, text).map_err(|e| RunError::Io(overlay.clone(), e))?;
            let g = self.group(&stack, &name);
            if self.network == crate::policy::Network::Internet {
                // Its networks exist, nothing runs yet: the rules first.
                dc.create(&g)?;
                for n in project_networks(&g) {
                    if !networks.contains(&n) {
                        networks.push(n);
                    }
                }
                crate::egress::apply(&self.id, &subnets(&networks)).map_err(RunError::Docker)?;
            }
            dc.up(&g, Duration::from_secs(300))?;
            // Ports and environments as the stack file's Compose files have them
            // (`model`, read before the overlay that publishes nothing): the lane
            // container reaches every target port by the service's name.
            let ps = dc.ps(&g)?;
            // The networks the group's containers are on now, as Docker has
            // them: a project with networks of its own has no `_default`.
            for n in project_networks(&g) {
                if !networks.contains(&n) {
                    networks.push(n);
                }
            }
            let services: BTreeMap<String, serde_json::Value> = model
                .services
                .iter()
                .map(|(svc, m)| {
                    let c = ps.iter().find(|c| &c.service == svc);
                    let ports: BTreeMap<String, String> = m
                        .ports
                        .iter()
                        .map(|p| (p.target.to_string(), format!("{svc}:{}", p.target)))
                        .collect();
                    // Where a local lane would publish each port: the host
                    // port of the Compose files, moved by the lane's offset.
                    let published: BTreeMap<String, u16> = m
                        .ports
                        .iter()
                        .filter_map(|p| {
                            let host: u16 = p.published.as_deref()?.trim().parse().ok()?;
                            Some((p.target.to_string(), host.checked_add(self.port_offset)?))
                        })
                        .collect();
                    if !c.map(|c| c.state.clone()).unwrap_or_default().is_empty() {
                        infra_ports.extend(published.values().copied());
                    }
                    (
                        svc.clone(),
                        serde_json::json!({
                            "environment": m.environment,
                            "ports": ports,
                            "published": published,
                            "state": c.map(|c| c.state.clone()).unwrap_or_default(),
                            "health": c.map(|c| c.health.clone()).unwrap_or_default(),
                        }),
                    )
                })
                .collect();
            infra.insert(
                name.clone(),
                serde_json::json!({ "project": g.project, "services": services }),
            );
        }
        let state = self.src().join(".devstack");
        std::fs::create_dir_all(state.join("xdg")).map_err(|e| RunError::Io(state.clone(), e))?;
        let file = state.join("external-infra.json");
        std::fs::write(
            &file,
            serde_json::to_vec_pretty(&serde_json::json!({ "groups": infra }))
                .expect("infra serialises"),
        )
        .map_err(|e| RunError::Io(file.clone(), e))?;

        // A lane without infrastructure gets a network of its own: never
        // Docker's default one, where the node's policy could not hold it.
        if networks.is_empty() {
            let own = self.own_network();
            if docker(&["network", "inspect", &own]).is_err() {
                let mut a = vec!["network", "create", "--label", "devstack.lane=1"];
                if self.network == crate::policy::Network::None {
                    a.push("--internal");
                }
                a.push(&own);
                docker(&a)?;
            }
            networks.push(own);
            if self.network == crate::policy::Network::Internet {
                crate::egress::apply(&self.id, &subnets(&networks)).map_err(RunError::Docker)?;
            }
        }

        let name = self.container();
        phase("lane container");
        let _ = docker(&["rm", "-f", &name]);
        let src = self.src().to_string_lossy().into_owned();
        let mount_src = format!("{src}:/src");
        let mount_bin = format!("{}:/opt/devstack/devstack:ro", bin.display());
        let stack_in = format!("/src/{}", self.stack_file);
        let listen = format!("DEVSTACK_LISTEN=0.0.0.0:{DAEMON_PORT}");
        let lane_env = format!("DEVSTACK_REMOTE_LANE={}", self.lane);
        let offset = format!("DEVSTACK_PORT_OFFSET={}", self.port_offset);
        let display = format!(
            "DEVSTACK_DISPLAY_ROOT={}",
            self.display_root.as_deref().unwrap_or("")
        );
        let label = format!("devstack.lease={}", self.id);
        // The lane runs as the agent's own user: what it writes into the
        // mirror (its state, its token, build output) stays the agent's to
        // read and to clean up. Its home (toolchain caches) is the lane's too.
        let home = self.dir.join("home");
        std::fs::create_dir_all(&home).map_err(|e| RunError::Io(home.clone(), e))?;
        let mount_home = format!("{}:/home/lane", home.display());
        let user = own_user();
        let memory = self.memory_limit.map(|m| format!("{m}b"));
        let cpus = self.cpu_limit.map(|c| format!("{c:.2}"));
        let mut args: Vec<&str> = vec![
            "run",
            "-d",
            "--name",
            &name,
            "--label",
            &label,
            "-v",
            &mount_home,
            "-e",
            "HOME=/home/lane",
            "--restart",
            "unless-stopped",
            "-w",
            "/src",
            "-v",
            &mount_src,
            "-v",
            &mount_bin,
            "-e",
            "DEVSTACK_EXTERNAL_INFRA=/src/.devstack/external-infra.json",
            "-e",
            &lane_env,
            "-e",
            &listen,
            "-e",
            "DEVSTACK_NO_HUB=1",
            "-e",
            "DEVSTACK_NO_UPDATE_CHECK=1",
            "-e",
            "XDG_STATE_HOME=/src/.devstack/xdg",
            "-e",
            &display,
            "-e",
            &offset,
        ];
        if let Some(n) = networks.first() {
            args.extend(["--network", n.as_str()]);
        }
        if let Some(u) = &user {
            args.extend(["--user", u.as_str()]);
        }
        if let Some(m) = &memory {
            args.extend(["--memory", m.as_str()]);
        }
        if let Some(c) = &cpus {
            args.extend(["--cpus", c.as_str()]);
        }
        args.extend([
            image.as_str(),
            "/opt/devstack/devstack",
            "daemon",
            "-f",
            &stack_in,
        ]);
        docker(&args)?;
        for n in networks.iter().skip(1) {
            docker(&["network", "connect", n, &name])?;
        }
        let addr = self.address(&networks, agent)?;
        phase("the lane's daemon");
        let token = self.wait_token(Duration::from_secs(60))?;
        infra_ports.sort_unstable();
        infra_ports.dedup();
        Ok(DaemonAddr {
            addr,
            token,
            infra_ports,
        })
    }

    /// The networks the lane container is on.
    fn networks(&self) -> Result<Vec<String>> {
        let text = docker(&[
            "inspect",
            "--format",
            "{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
            &self.container(),
        ])?;
        Ok(text.split_whitespace().map(str::to_string).collect())
    }

    /// Where the agent reaches the lane's daemon: by the container's name on
    /// the lane's network when the agent runs in a container (it joins that
    /// network), else at the container's address.
    fn address(&self, networks: &[String], agent: Option<&str>) -> Result<String> {
        let name = self.container();
        match agent {
            Some(me) => {
                for n in networks {
                    if let Err(e) = docker(&["network", "connect", n, me])
                        && !e.to_string().contains("already exists")
                    {
                        return Err(e);
                    }
                }
                Ok(format!("{name}:{DAEMON_PORT}"))
            }
            None => {
                let ip = docker(&[
                    "inspect",
                    "--format",
                    "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
                    &name,
                ])?;
                let ip = ip.split_whitespace().next().unwrap_or("").to_string();
                if ip.is_empty() {
                    return Err(RunError::Docker(format!("{name} has no address")));
                }
                Ok(format!("{ip}:{DAEMON_PORT}"))
            }
        }
    }

    /// A lane that runs already (the agent restarted): where its daemon is.
    pub fn attach(&self, agent: Option<&str>) -> Result<DaemonAddr> {
        if !self.running() {
            return Err(RunError::Docker("the lane is not running".into()));
        }
        let networks = self.networks()?;
        Ok(DaemonAddr {
            addr: self.address(&networks, agent)?,
            token: self.wait_token(Duration::from_secs(5))?,
            infra_ports: Vec::new(),
        })
    }

    fn token_file(&self) -> PathBuf {
        self.src().join(".devstack/xdg/devstack/token")
    }

    /// The token the lane's daemon created at its first start.
    fn wait_token(&self, within: Duration) -> Result<String> {
        let until = std::time::Instant::now() + within;
        loop {
            if let Ok(t) = std::fs::read_to_string(self.token_file())
                && !t.trim().is_empty()
            {
                return Ok(t.trim().to_string());
            }
            if std::time::Instant::now() > until {
                let logs = docker(&["logs", "--tail", "20", &self.container()]).unwrap_or_default();
                return Err(RunError::Docker(format!(
                    "the lane's daemon did not start within {} s: {logs}",
                    within.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Whether the lane container runs.
    pub fn running(&self) -> bool {
        docker(&[
            "inspect",
            "--format",
            "{{.State.Running}}",
            &self.container(),
        ])
        .is_ok_and(|s| s == "true")
    }

    /// The network of a lane without infrastructure.
    fn own_network(&self) -> String {
        format!("devstack-lane-{}", self.id)
    }

    /// Stop the lane: its container and its Compose projects, volumes
    /// included (a remote lane is disposable, ADR-0025 point 5).
    pub fn stop(&self, agent: Option<&str>) {
        let _ = docker(&["rm", "-f", &self.container()]);
        let own = self.own_network();
        if let Some(me) = agent {
            let _ = docker(&["network", "disconnect", "-f", &own, me]);
        }
        let _ = docker(&["network", "rm", &own]);
        crate::egress::remove(&self.id);
        let Ok(stack) = self.stack() else { return };
        let dc = Docker::default();
        for name in order(&stack).into_iter().rev() {
            let g = self.group(&stack, &name);
            // The agent leaves the lane's networks first: Docker does not
            // remove a network something is still on.
            if let Some(me) = agent {
                for n in project_networks(&g) {
                    let _ = docker(&["network", "disconnect", "-f", &n, me]);
                }
            }
            let _ = dc.remove(&g);
        }
    }
}

/// The subnets of Docker networks.
fn subnets(networks: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for n in networks {
        let text = docker(&[
            "network",
            "inspect",
            "-f",
            "{{range .IPAM.Config}}{{.Subnet}} {{end}}",
            n,
        ])
        .unwrap_or_default();
        for s in text.split_whitespace() {
            // IPv4 only: the rules are iptables'.
            if s.contains('.') && !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// `uid:gid` of this process (Linux), for the lane container to run as.
fn own_user() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .map(str::to_string)
    };
    Some(format!("{}:{}", field("Uid:")?, field("Gid:")?))
}

/// The networks of a group's Compose project (by Compose's project label),
/// and the group's own fixed-name networks: what a lane container joins.
fn project_networks(g: &Group) -> Vec<String> {
    let label = format!("label=com.docker.compose.project={}", g.project);
    let mut out: Vec<String> =
        docker(&["network", "ls", "--filter", &label, "--format", "{{.Name}}"])
            .map(|t| {
                t.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
    for n in &g.networks {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}
