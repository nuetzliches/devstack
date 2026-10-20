//! A lane running on the node (ADR-0025 point 2, ADR-0028): the lane's
//! infrastructure as Compose projects of its own on the node's Docker
//! engine, and one lane container from the stack's lane image, in which the
//! node's own static Linux `devstack` runs the lane's daemon from the mirror.
//! The daemon there has no Docker: the agent describes the infrastructure to
//! it in a file (`DEVSTACK_EXTERNAL_INFRA`, spike M0).
//!
//! The mirror is the requester's, and the lane's own code writes into it:
//! everything the agent reads or writes there goes through [`crate::safefs`]
//! (no symlink is followed), and what the Compose files ask of the node is
//! checked first ([`crate::confine`]).
//!
//! Everything here calls the Docker CLI and blocks.

use crate::policy::Network;
use crate::safefs;
use devstack_compose::{Docker, Group, Model, budget};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The port the lane's daemon listens on inside its container.
pub const DAEMON_PORT: u16 = 7600;
/// The most processes one container of a lane may run.
pub const PIDS_LIMIT: u32 = 4096;

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
    /// The lease id: names its container, its own network, its egress rules.
    pub id: String,
    /// Its slot (ADR-0038): names its Compose projects, their networks and
    /// volumes, which the next lease of the same lane finds again. A lease
    /// from an older agent has its id here.
    pub slot: String,
    /// The requester's member id: the images built for it are its own.
    pub member: String,
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
    /// The profile it runs with (ADR-0041, `DEVSTACK_PROFILE`).
    pub profile: Option<String>,
    /// The memory limit of each of its containers (ADR-0033), bytes.
    pub memory_limit: Option<u64>,
    /// Their CPU limit, cores.
    pub cpu_limit: Option<f64>,
    /// What its networks reach beyond the lane (ADR-0033 point 5).
    pub network: Network,
    /// A secret of this node, so names of images built here cannot be
    /// guessed by another lane (and used in its `FROM` or `image:`).
    pub secret: String,
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

fn docker_for(args: &[&str], within: Duration) -> Result<String> {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    let out = devstack_compose::output_within(cmd, within)
        .map_err(|e| RunError::Docker(format!("run docker: {e}")))?
        .ok_or_else(|| {
            RunError::Docker(format!(
                "docker {} did not finish within {} s",
                args.first().copied().unwrap_or(""),
                within.as_secs()
            ))
        })?;
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

fn docker(args: &[&str]) -> Result<String> {
    docker_for(args, budget::QUICK)
}

/// Pull `image` unless the node has it.
fn ensure_image(image: &str) -> Result<()> {
    if docker(&["image", "inspect", "--format", "{{.Id}}", image]).is_err() {
        docker_for(&["pull", image], budget::PULL)?;
    }
    Ok(())
}

/// A name for something built for `member` that only this node can tell.
pub(crate) fn secret_name(secret: &str, parts: &[&str]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(secret.as_bytes());
    for p in parts {
        h.update(&[0]);
        h.update(p.as_bytes());
    }
    h.finalize().to_hex()[..24].to_string()
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

/// What the lane's daemon writes and the agent reads back: its token.
const TOKEN: &str = ".devstack/xdg/devstack/token";

impl LaneRun {
    pub fn src(&self) -> PathBuf {
        self.dir.join("src")
    }

    /// The node agent's own state (`node/`), two levels above the lane.
    fn state(&self) -> PathBuf {
        self.dir
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| self.dir.clone(), Path::to_path_buf)
    }

    fn container(&self) -> String {
        format!("devstack-lane-{}", self.id)
    }

    /// The stack file, read without following a link the lane put there.
    pub(crate) fn stack(&self) -> Result<devstack_config::Stack> {
        let rel = Path::new(&self.stack_file);
        let text = safefs::read_beneath(&self.src(), rel, 1 << 20)
            .map_err(|e| RunError::Config(format!("the stack file `{}`: {e}", self.stack_file)))?;
        let text = String::from_utf8(text)
            .map_err(|_| RunError::Config(format!("`{}` is not UTF-8", self.stack_file)))?;
        devstack_config::from_str(&text, &self.src().join(rel))
            .map_err(|e| RunError::Config(e.to_string()))
    }

    /// The lane's copy of a group: its own project, its own fixed-name
    /// networks, nothing shared (ADR-0025 point 3), the node overlay once it
    /// is written.
    fn group(&self, stack: &devstack_config::Stack, name: &str) -> Group {
        let cfg = &stack.compose[name];
        let mut g = Group::from_config(name, cfg, &self.src());
        g.project = format!("{}-{}", cfg.project, self.slot);
        g.networks = cfg
            .networks
            .iter()
            .map(|n| format!("{n}-{}", self.slot))
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

    /// The group's model as `docker compose config` reads it in a container
    /// that sees the mirror only ([`crate::confine`]).
    fn config(&self, g: &Group, flags: &[&str]) -> Result<Model> {
        let image = crate::confine::config_image();
        ensure_image(&image)?;
        let args = crate::confine::config_args(g, &self.src(), &image, flags);
        let mut cmd = Command::new("docker");
        cmd.args(&args);
        let out = devstack_compose::output_within(cmd, budget::QUICK)
            .map_err(|e| RunError::Docker(format!("run docker: {e}")))?
            .ok_or_else(|| RunError::Docker("`compose config` did not finish".into()))?;
        if !out.status.success() {
            return Err(RunError::Config(format!(
                "compose group `{}`: {}",
                g.name,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        devstack_compose::parse_config(&String::from_utf8_lossy(&out.stdout))
            .map_err(|e| RunError::Config(format!("compose group `{}`: {e}", g.name)))
    }

    /// The image the lane runs in: the stack's `[lanes] image`, pulled if the
    /// node lacks it, or one built from its `[lanes] dockerfile` and kept by
    /// the Dockerfile's content, for this member (ADR-0028). The build has
    /// no network unless the node's is `lan`.
    fn image(&self, stack: &devstack_config::Stack) -> Result<String> {
        if let Some(img) = &stack.lanes.image {
            ensure_image(img)?;
            return Ok(img.clone());
        }
        let Some(df) = &stack.lanes.dockerfile else {
            return Err(RunError::Config(
                "the stack names no lane image: set `[lanes] image` or `[lanes] dockerfile` (ADR-0028)"
                    .into(),
            ));
        };
        let base_rel = Path::new(&self.stack_file)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let text = safefs::read_beneath(&self.src(), &base_rel.join(df), 1 << 20)
            .map_err(|e| RunError::Config(format!("[lanes] dockerfile `{}`: {e}", df.display())))?;
        let base = safefs::resolve_beneath(&self.src(), &self.src().join(&base_rel))
            .map_err(RunError::Config)?;
        let content = blake3::hash(&text).to_hex().to_string();
        let tag = format!(
            "devstack-lane:{}",
            secret_name(&self.secret, &[&self.member, &content])
        );
        if docker(&["image", "inspect", "--format", "{{.Id}}", &tag]).is_err() {
            // Docker reads the Dockerfile from a copy the lane cannot change.
            let file = self.dir.join("lane.Dockerfile");
            std::fs::write(&file, &text).map_err(|e| RunError::Io(file.clone(), e))?;
            let file = file.to_string_lossy().into_owned();
            let base = base.to_string_lossy().into_owned();
            let mut a = vec!["build", "-q", "-t", &tag, "-f", &file];
            if self.network != Network::Lan {
                a.extend(["--network", "none"]);
            }
            a.push(&base);
            docker_for(&a, budget::PULL)?;
        }
        Ok(tag)
    }

    /// Create `name`, a network of the lane's own, as the node's policy
    /// wants it: internal for `none`, IPv4 only unless `lan`.
    fn own_network_create(&self, name: &str) -> Result<()> {
        let mut flags = vec!["--label", "devstack.lane=1"];
        if self.network == Network::None {
            flags.push("--internal");
        }
        if self.network != Network::Lan {
            flags.push("--ipv6=false");
        }
        Docker::default()
            .ensure_network(&self.dir, name, &flags)
            .map_err(RunError::from)
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
        // Nothing of the lane runs while the agent reads its files.
        let _ = docker(&["rm", "-f", &self.container()]);
        let stack = self.stack()?;
        phase("lane image: pulled or built once, then kept");
        let image = self.image(&stack)?;
        let dc = Docker::default();
        let src = self.src();

        // Every group checked, its overlay written and its containers
        // created; nothing runs yet, so the network rules go in first.
        let mut groups: Vec<(String, Group, Model)> = Vec::new();
        for name in order(&stack) {
            phase(&format!("infrastructure: {name}: checking"));
            let overlay = self.overlay(&name);
            let _ = std::fs::remove_file(&overlay);
            let g = self.group(&stack, &name);
            crate::confine::check_group(&g, &src).map_err(RunError::Config)?;
            let own = &stack.compose[&name].networks;
            let paths = self.config(&g, &["--no-env-resolution"])?;
            crate::confine::check_model(&name, &paths, &src, self.network, own, &g.services)
                .map_err(RunError::Config)?;
            let model = self.config(&g, &[])?;
            let options = devstack_compose::NodeOptions {
                own_networks: own.clone(),
                isolated: self.network == Network::None,
                no_ipv6: self.network != Network::Lan,
                build_offline: self.network != Network::Lan,
                memory: self.memory_limit,
                cpus: self.cpu_limit,
                pids: Some(PIDS_LIMIT),
                build_images: model
                    .services
                    .iter()
                    .filter(|(_, s)| s.build.is_some())
                    .map(|(svc, _)| {
                        (
                            svc.clone(),
                            format!(
                                "devstack-build-{}",
                                secret_name(&self.secret, &[&self.member, &g.project, svc])
                            ),
                        )
                    })
                    .collect(),
            };
            let text = devstack_compose::node_overlay(&model, &g.project, &self.slot, &options)
                .map_err(|e| RunError::Config(format!("compose group `{name}`: {e}")))?;
            std::fs::create_dir_all(overlay.parent().expect("has a parent"))
                .map_err(|e| RunError::Io(overlay.clone(), e))?;
            std::fs::write(&overlay, text).map_err(|e| RunError::Io(overlay.clone(), e))?;
            let g = self.group(&stack, &name);
            for n in &g.networks {
                self.own_network_create(n)?;
            }
            // Pulls and builds what is missing: the slow part of a first
            // start, timed apart from starting and waiting.
            phase(&format!(
                "infrastructure: {name}: images (pull, build) and containers"
            ));
            dc.create(&g)?;
            groups.push((name, g, model));
        }
        let mut networks = Vec::new();
        for (_, g, _) in &groups {
            for n in project_networks(g) {
                if !networks.contains(&n) {
                    networks.push(n);
                }
            }
        }
        // A lane without infrastructure gets a network of its own: never
        // Docker's default one, where the node's policy could not hold it.
        if networks.is_empty() {
            let own = self.own_network();
            self.own_network_create(&own)?;
            networks.push(own);
        }
        if self.network != Network::Lan {
            crate::egress::apply(&self.state(), &self.id, &bridges(&networks), self.network)
                .map_err(RunError::Docker)?;
        }

        let mut infra = BTreeMap::new();
        let mut infra_ports = Vec::new();
        // A profile with `data = "fresh"` (ADR-0041): its slot's volumes go
        // before its containers start (ADR-0038 keeps them otherwise).
        let fresh = self
            .profile
            .as_ref()
            .and_then(|p| stack.profiles.get(p))
            .is_some_and(|p| p.data == Some(devstack_config::model::DataMode::Fresh));
        for (name, g, model) in &groups {
            if fresh {
                phase(&format!(
                    "infrastructure: {name}: its data goes (a fresh start)"
                ));
                let _ = dc.remove(g);
            }
            // Started, then waited for, as two phases: a slow container
            // start and a slow health check are told apart.
            phase(&format!("infrastructure: {name}: starting containers"));
            dc.up_detached(g)?;
            phase(&format!("infrastructure: {name}: waiting until healthy"));
            dc.up(g, Duration::from_secs(300))?;
            // Ports and environments as the stack file's Compose files have them
            // (`model`, read before the overlay that publishes nothing): the lane
            // container reaches every target port by the service's name.
            let ps = dc.ps(g)?;
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
        let io = |p: &str| {
            let p = src.join(p);
            move |e| RunError::Io(p, e)
        };
        let infra_json = serde_json::to_vec_pretty(&serde_json::json!({ "groups": infra }))
            .expect("infra serialises");
        safefs::write_beneath(
            &src,
            Path::new(".devstack/external-infra.json"),
            0o644,
            |f| std::io::Write::write_all(f, &infra_json),
        )
        .map_err(io(".devstack/external-infra.json"))?;
        // The agent's own copy, beside the mirror where the lane cannot
        // write: the projects it started, and what it refreshes the
        // daemon's file from (`laneinfra`).
        std::fs::write(self.dir.join(crate::laneinfra::AGENT_FILE), &infra_json)
            .map_err(|e| RunError::Io(self.dir.join(crate::laneinfra::AGENT_FILE), e))?;
        // The daemon's state directory, and no token of an earlier start.
        safefs::write_beneath(&src, Path::new(".devstack/xdg/.keep"), 0o644, |_| Ok(()))
            .map_err(io(".devstack/xdg"))?;
        let _ = safefs::remove_beneath(&src, Path::new(TOKEN));

        let name = self.container();
        phase("lane container");
        let src_s = src.to_string_lossy().into_owned();
        let mount_src = format!("{src_s}:/src");
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
        let pids = PIDS_LIMIT.to_string();
        let mut args: Vec<&str> = vec![
            "run",
            "-d",
            "--name",
            &name,
            "--label",
            &label,
            // No privilege it could gain (ADR-0027 point 5).
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            &pids,
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
        let profile_env = self
            .profile
            .as_ref()
            .map(|p| format!("DEVSTACK_PROFILE={p}"));
        if let Some(p) = &profile_env {
            args.extend(["-e", p.as_str()]);
        }
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

    /// The token the lane's daemon created at its first start: read without
    /// following a link (the agent sends it to the lane: a link to one of
    /// the agent's own files would hand that file to the lane).
    fn wait_token(&self, within: Duration) -> Result<String> {
        let until = std::time::Instant::now() + within;
        loop {
            if let Ok(t) = safefs::read_beneath(&self.src(), Path::new(TOKEN), 512) {
                let t = String::from_utf8_lossy(&t).trim().to_string();
                if !t.is_empty() {
                    if !valid_token(&t) {
                        return Err(RunError::Docker(
                            "the lane's daemon wrote a token that is none".into(),
                        ));
                    }
                    return Ok(t);
                }
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

    /// Stop the lane: its container and its Compose projects' containers.
    /// Their volumes and the mirror stay for the next start (ADR-0038).
    pub fn stop(&self, agent: Option<&str>) {
        self.take_down(agent, false);
    }

    /// Stop the lane and remove what it keeps: its Compose projects'
    /// volumes and its directory (the mirror, its builds, its caches).
    pub fn purge(&self, agent: Option<&str>) {
        self.take_down(agent, true);
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Whether its stack file asks to keep its data (`[lanes] keep_data`);
    /// a file that does not read keeps it.
    pub fn keeps_data(&self) -> bool {
        self.stack().map_or(true, |s| s.lanes.keep_data)
    }

    fn take_down(&self, agent: Option<&str>, volumes: bool) {
        let _ = docker(&["rm", "-f", &self.container()]);
        let own = self.own_network();
        if let Some(me) = agent {
            let _ = docker(&["network", "disconnect", "-f", &own, me]);
        }
        let _ = docker(&["network", "rm", &own]);
        crate::egress::remove(&self.state(), &self.id);
        let Ok(stack) = self.stack() else { return };
        let dc = Docker::default();
        for name in order(&stack).into_iter().rev() {
            let g = self.group(&stack, &name);
            // Compose reads the files again: only those inside the mirror.
            if crate::confine::check_group(&g, &self.src()).is_err() {
                continue;
            }
            // The agent leaves the lane's networks first: Docker does not
            // remove a network something is still on.
            if let Some(me) = agent {
                for n in project_networks(&g) {
                    let _ = docker(&["network", "disconnect", "-f", &n, me]);
                }
            }
            let _ = if volumes { dc.remove(&g) } else { dc.down(&g) };
        }
    }
}

/// A daemon's token is hex (ADR-0019); anything else is not one.
fn valid_token(t: &str) -> bool {
    t.len() <= 256
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Each network's IPv4 subnets with the bridge interface they are on.
fn bridges(networks: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for n in networks {
        let text = docker(&[
            "network",
            "inspect",
            "-f",
            "{{.Id}}|{{index .Options \"com.docker.network.bridge.name\"}}|{{range .IPAM.Config}}{{.Subnet}} {{end}}",
            n,
        ])
        .unwrap_or_default();
        out.extend(parse_bridges(&text));
    }
    out.sort();
    out.dedup();
    out
}

/// `<id>|<bridge name or empty>|<subnets>` as [`bridges`] asks for it:
/// IPv4 subnets (the rules are iptables') and the bridge (Docker names one
/// `br-<12 digits of the id>` unless the network names it).
fn parse_bridges(text: &str) -> Vec<(String, String)> {
    let mut parts = text.trim().splitn(3, '|');
    let (Some(id), Some(name), Some(subnets)) = (parts.next(), parts.next(), parts.next()) else {
        return Vec::new();
    };
    let bridge = match name.trim() {
        "" | "<no value>" => format!("br-{}", id.get(..12).unwrap_or(id)),
        n => n.to_string(),
    };
    subnets
        .split_whitespace()
        .filter(|s| s.contains('.'))
        .map(|s| (s.to_string(), bridge.clone()))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_is_known_by_its_bridge() {
        let id = "0123456789abcdef0123";
        assert_eq!(
            parse_bridges(&format!("{id}||172.30.0.0/16 fd00::/64 ")),
            [("172.30.0.0/16".to_string(), "br-0123456789ab".to_string())]
        );
        assert_eq!(
            parse_bridges(&format!("{id}|lane0|10.9.0.0/24")),
            [("10.9.0.0/24".to_string(), "lane0".to_string())]
        );
        assert!(parse_bridges("").is_empty());
    }

    #[test]
    fn names_built_for_a_member_are_its_own() {
        let a = secret_name("s1", &["m1", "shop-ab", "api"]);
        assert_eq!(a.len(), 24);
        assert_ne!(a, secret_name("s1", &["m2", "shop-ab", "api"]));
        assert_ne!(a, secret_name("s2", &["m1", "shop-ab", "api"]));
        assert!(valid_token("0123abcdef") && !valid_token("-----BEGIN PRIVATE KEY-----\nx"));
    }
}
