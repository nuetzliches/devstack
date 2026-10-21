//! A lane running on the node (ADR-0025 point 2, ADR-0028): the lane's
//! infrastructure as Compose projects of its own on the node's Docker
//! engine, and one lane container from the stack's lane image, in which the
//! node's own static Linux `devstack` runs the lane's daemon from the mirror.
//! The daemon there has no Docker: the agent describes the infrastructure to
//! it in a file (`DEVSTACK_EXTERNAL_INFRA`, spike M0). The daemon starts
//! first; the agent starts the containers under it once it answered (#164).
//!
//! The mirror is the requester's, and the lane's own code writes into it:
//! everything the agent reads or writes there goes through [`crate::safefs`]
//! (no symlink is followed), and what the Compose files ask of the node is
//! checked first ([`crate::confine`]). Compose on the node then runs the
//! model the agent checked, from `compose/` beside the mirror, never the
//! lane's files: nothing the lane changes after the check reaches it.
//!
//! Everything here calls the Docker CLI and blocks, with only what Docker
//! needs of the agent's environment ([`devstack_compose::clean_env`]).

use crate::policy::Network;
use crate::safefs;
use devstack_compose::{Docker, Group, Model, budget};
use serde::{Deserialize, Serialize};
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

/// A lane whose daemon answers: where it is, and what is left of its start.
pub struct Started {
    pub addr: DaemonAddr,
    pub infra: InfraUp,
}

/// What a start leaves for after the lane's daemon answered (#164): its
/// Compose groups' containers, created and checked, started and waited for
/// in order, as a local lane's daemon does. The daemon waits for them.
pub struct InfraUp {
    groups: Vec<(String, Group)>,
}

/// How long a group's containers may take to be healthy once started.
pub const HEALTHY_WITHIN: Duration = Duration::from_secs(300);

impl InfraUp {
    /// The groups, in the order they start.
    pub fn groups(&self) -> Vec<String> {
        self.groups.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Start each group and wait until it is healthy, in order, its state
    /// written into the lane's files as it goes (`laneinfra`). A group that
    /// fails is written as failed with its error, the groups after it as
    /// not started, and returned. `stop`: the lane stops or starts afresh
    /// (Compose ends at once, nothing more is written); `done` hears each
    /// group's outcome and how long it took.
    pub fn run(
        &self,
        lane: &LaneRun,
        stop: &dyn Fn() -> bool,
        done: &dyn Fn(&str, Duration, Option<&str>),
    ) -> std::result::Result<(), String> {
        let dc = Docker::clean();
        let src = lane.src();
        let set = |group: &str, state: &str, error: Option<&str>| {
            if let Err(e) = crate::laneinfra::set_group(&lane.dir, &src, group, state, error) {
                eprintln!("lane {}: compose group `{group}`: {e}", lane.id);
            }
        };
        for (i, (name, g)) in self.groups.iter().enumerate() {
            if stop() {
                return Ok(());
            }
            let t = std::time::Instant::now();
            set(name, crate::laneinfra::STARTING, None);
            // Nothing is built here: the lane runs by now, and its build
            // contexts are its to change (ADR-0033 point 11).
            let started = dc.start_created(g, Some(HEALTHY_WITHIN), stop);
            if stop() {
                return Ok(());
            }
            match started {
                Ok(()) => {
                    set(name, crate::laneinfra::UP, None);
                    done(name, t.elapsed(), None);
                }
                Err(e) => {
                    let e = e.to_string();
                    set(name, crate::laneinfra::FAILED, Some(&e));
                    done(name, t.elapsed(), Some(&e));
                    let why = format!("not started: compose group `{name}` failed");
                    for (rest, _) in &self.groups[i + 1..] {
                        set(rest, crate::laneinfra::FAILED, Some(&why));
                    }
                    return Err(format!("compose group `{name}`: {e}"));
                }
            }
        }
        Ok(())
    }
}

/// What the lane's daemon reads of its infrastructure before anything of it
/// runs (#164): each group `created`, each service's environment and the
/// addresses of its ports as the stack file's Compose files have them
/// (`model`, read before the overlay that publishes nothing): the lane
/// container reaches every target port by the service's name. And the ports
/// the requester forwards: where a local lane would publish them.
fn initial_infra(
    groups: &[(String, Group, Model)],
    port_offset: u16,
) -> (serde_json::Value, Vec<u16>) {
    let mut infra = BTreeMap::new();
    let mut infra_ports = Vec::new();
    for (name, g, model) in groups {
        let services: BTreeMap<String, serde_json::Value> = model
            .services
            .iter()
            .map(|(svc, m)| {
                // What the group starts; the rest is parked (an environment
                // source only), without a state.
                let starts = g.services.is_empty() || g.services.contains(svc);
                let ports: BTreeMap<String, String> = m
                    .ports
                    .iter()
                    .map(|p| (p.target.to_string(), format!("{svc}:{}", p.target)))
                    .collect();
                // Where a local lane would publish each port: the host port
                // of the Compose files, moved by the lane's offset.
                let published: BTreeMap<String, u16> = m
                    .ports
                    .iter()
                    .filter_map(|p| {
                        let host: u16 = p.published.as_deref()?.trim().parse().ok()?;
                        Some((p.target.to_string(), host.checked_add(port_offset)?))
                    })
                    .collect();
                if starts {
                    infra_ports.extend(published.values().copied());
                }
                (
                    svc.clone(),
                    serde_json::json!({
                        "environment": m.environment,
                        "ports": ports,
                        "published": published,
                        "state": if starts { "created" } else { "" },
                        "health": "",
                    }),
                )
            })
            .collect();
        infra.insert(
            name.clone(),
            serde_json::json!({
                "project": g.project,
                "state": crate::laneinfra::CREATED,
                "services": services,
            }),
        );
    }
    infra_ports.sort_unstable();
    infra_ports.dedup();
    (serde_json::json!({ "groups": infra }), infra_ports)
}

fn docker_for(args: &[&str], within: Duration) -> Result<String> {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    devstack_compose::clean_env(&mut cmd);
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

/// What tells this agent's lane containers, images and network rules from
/// another agent's on the same Docker engine (one agent per pool,
/// ADR-0034): 8 hex digits of its secret, the `devstack.agent` label.
pub(crate) fn owner_tag(secret: &str) -> String {
    secret_name(secret, &["agent"])[..8].to_string()
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

/// The Compose projects of a lane, in [`LaneRun::compose_dir`].
const PROJECTS: &str = "projects.json";

/// A Compose project of a lane: what the agent takes down at its end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Project {
    project: String,
    /// The group's own fixed-name networks (`[compose.<group>] networks`).
    #[serde(default)]
    networks: Vec<String>,
}

/// The ids of the containers of Compose project `project`, by Compose's
/// label: whatever files made them.
fn project_containers(project: &str) -> Vec<String> {
    by_label(&["ps", "-aq"], project)
}

/// `docker <ls> --filter label=com.docker.compose.project=<project>`, one
/// name or id a line.
fn by_label(ls: &[&str], project: &str) -> Vec<String> {
    let label = format!("label=com.docker.compose.project={project}");
    let mut args = ls.to_vec();
    args.extend(["--filter", &label]);
    docker(&args)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Stop the containers of `p` (they stay, with their volumes).
fn stop_project(p: &Project) {
    let ids = project_containers(&p.project);
    if !ids.is_empty() {
        let mut a = vec!["stop", "--timeout", "10"];
        a.extend(ids.iter().map(String::as_str));
        let _ = docker_for(&a, budget::STOP);
    }
}

/// Remove `p`'s containers and networks, as `compose down` does, by
/// Compose's labels: what the lane started, whatever its files say now.
/// `volumes`: its volumes and its own fixed-name networks too.
fn remove_project(p: &Project, volumes: bool) {
    let ids = project_containers(&p.project);
    if !ids.is_empty() {
        stop_project(p);
        let mut a = vec!["rm", "-f"];
        if volumes {
            a.push("-v");
        }
        a.extend(ids.iter().map(String::as_str));
        let _ = docker_for(&a, budget::STOP);
    }
    for n in by_label(&["network", "ls", "-q"], &p.project) {
        let _ = docker(&["network", "rm", &n]);
    }
    if volumes {
        for v in by_label(&["volume", "ls", "-q"], &p.project) {
            let _ = docker(&["volume", "rm", "-f", &v]);
        }
        for n in &p.networks {
            let _ = docker(&["network", "rm", n]);
        }
    }
}

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

    /// Where the agent keeps what it runs of the lane's groups: beside the
    /// mirror, where the lane does not write.
    fn compose_dir(&self) -> PathBuf {
        self.dir.join("compose")
    }

    /// A group's project on this node, and its own fixed-name networks:
    /// nothing shared (ADR-0025 point 3).
    fn project(&self, stack: &devstack_config::Stack, name: &str) -> Project {
        let cfg = &stack.compose[name];
        Project {
            project: format!("{}-{}", cfg.project, self.slot),
            networks: cfg
                .networks
                .iter()
                .map(|n| format!("{n}-{}", self.slot))
                .collect(),
        }
    }

    /// A group as the lane's files have it: what `config` reads, in a
    /// container that sees the mirror only.
    fn mirror_group(&self, stack: &devstack_config::Stack, name: &str) -> Group {
        let p = self.project(stack, name);
        let mut g = Group::from_config(name, &stack.compose[name], &self.src());
        g.project = p.project;
        g.networks = p.networks;
        g
    }

    /// A group as the node runs it: the model the agent checked and its
    /// overlay, both in [`LaneRun::compose_dir`], nothing of the mirror.
    fn group(&self, stack: &devstack_config::Stack, name: &str) -> Group {
        let p = self.project(stack, name);
        let dir = self.compose_dir();
        Group {
            name: name.to_string(),
            project: p.project,
            files: vec![
                dir.join(format!("{name}.json")),
                dir.join(format!("{name}.yml")),
            ],
            env_files: Vec::new(),
            networks: p.networks,
            services: stack.compose[name].services.clone(),
            dir,
        }
    }

    /// The Compose projects of this lane the agent knows of: each it ever
    /// started here (recorded in [`LaneRun::compose_dir`]: the lane's stack
    /// file may name others by now) and each `stack` names.
    fn projects(&self, stack: Option<&devstack_config::Stack>) -> Vec<Project> {
        let mut out: Vec<Project> = std::fs::read(self.compose_dir().join(PROJECTS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if let Some(stack) = stack {
            for name in order(stack) {
                let p = self.project(stack, &name);
                if !out.iter().any(|o| o.project == p.project) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Note `p` among the projects of this lane, before anything of it is
    /// created.
    fn record(&self, p: &Project) -> Result<()> {
        let file = self.compose_dir().join(PROJECTS);
        let mut all: Vec<Project> = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !all.contains(p) {
            all.push(p.clone());
            let text = serde_json::to_vec_pretty(&all).expect("projects serialise");
            std::fs::write(&file, text).map_err(|e| RunError::Io(file.clone(), e))?;
        }
        Ok(())
    }

    /// The group's model as `docker compose config` writes it in a
    /// container that sees the mirror only ([`crate::confine`]), without
    /// values left for the node's environment to fill in.
    fn config(&self, g: &Group) -> Result<serde_json::Value> {
        let image = crate::confine::config_image();
        ensure_image(&image)?;
        let args = crate::confine::config_args(g, &self.src(), &image, &[]);
        let mut cmd = Command::new("docker");
        cmd.args(&args);
        devstack_compose::clean_env(&mut cmd);
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
        let mut raw: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| RunError::Config(format!("compose group `{}`: {e}", g.name)))?;
        crate::confine::strip_unset(&mut raw);
        Ok(raw)
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
            // This agent's and the member's: removed with the member, or
            // under pressure (`node::remove_lane_images`).
            let owner = format!("devstack.agent={}", owner_tag(&self.secret));
            let member = format!("devstack.member={}", self.member);
            let mut a = vec![
                "build", "-q", "-t", &tag, "--label", &owner, "--label", &member, "-f", &file,
            ];
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
        Docker::clean()
            .ensure_network(&self.dir, name, &flags)
            .map_err(RunError::from)
    }

    /// Start the lane: its infrastructure checked, its images and containers
    /// made, their description for the daemon, the lane container; once its
    /// daemon answers, the containers are left to start ([`Started::infra`],
    /// #164). `bin` is a static Linux `devstack` the container mounts;
    /// `agent`, the agent's own container when it runs in one (it joins the
    /// lane's network to reach the daemon).
    /// `phase` hears what it does now ("image", "infrastructure: app", ...).
    pub fn start(&self, bin: &Path, agent: Option<&str>, phase: &dyn Fn(&str)) -> Result<Started> {
        // Nothing of the lane runs while the agent reads its files: not its
        // container, not the containers of an earlier start (one of them
        // could change the mirror between the check and the start).
        let _ = docker(&["rm", "-f", &self.container()]);
        let stack = self.stack()?;
        for p in self.projects(Some(&stack)) {
            stop_project(&p);
        }
        phase("lane image: pulled or built once, then kept");
        let image = self.image(&stack)?;
        let dc = Docker::clean();
        let src = self.src();
        let compose_dir = self.compose_dir();
        std::fs::create_dir_all(&compose_dir).map_err(|e| RunError::Io(compose_dir.clone(), e))?;
        // A profile with `data = "fresh"` (ADR-0041): its slot's volumes go
        // before its containers are made (ADR-0038 keeps them otherwise).
        let fresh = self
            .profile
            .as_ref()
            .and_then(|p| stack.profiles.get(p))
            .is_some_and(|p| p.data == Some(devstack_config::model::DataMode::Fresh));

        // Every group checked, the model the node runs and its overlay
        // written, its containers created; nothing runs yet, so the network
        // rules go in first.
        let mut groups: Vec<(String, Group, Model)> = Vec::new();
        for name in order(&stack) {
            phase(&format!("infrastructure: {name}: checking"));
            let mirror = self.mirror_group(&stack, &name);
            for f in [format!("{name}.json"), format!("{name}.yml")] {
                let _ = std::fs::remove_file(compose_dir.join(f));
            }
            crate::confine::check_group(&mirror, &src).map_err(RunError::Config)?;
            let own = &stack.compose[&name].networks;
            let raw = self.config(&mirror)?;
            let model: Model = serde_json::from_value(raw.clone())
                .map_err(|e| RunError::Config(format!("compose group `{name}`: {e}")))?;
            crate::confine::check(
                &name,
                &raw,
                &model,
                &src,
                self.network,
                own,
                &mirror.services,
            )
            .map_err(RunError::Config)?;
            // What Compose runs from here on: the model just checked.
            let rendered = crate::confine::render(
                raw,
                &model,
                &src,
                &mirror.services,
                &compose_dir.join("files").join(&name),
            )
            .map_err(|e| RunError::Config(format!("compose group `{name}`: {e}")))?;
            let file = compose_dir.join(format!("{name}.json"));
            // The overlay covers what the node runs, nothing more: an entry
            // for a service left out would be a service without an image.
            let ran = devstack_compose::parse_config(&rendered)
                .map_err(|e| RunError::Config(format!("compose group `{name}`: {e}")))?;
            std::fs::write(&file, rendered).map_err(|e| RunError::Io(file.clone(), e))?;
            let options = devstack_compose::NodeOptions {
                own_networks: own.clone(),
                isolated: self.network == Network::None,
                no_ipv6: self.network != Network::Lan,
                build_offline: self.network != Network::Lan,
                memory: self.memory_limit,
                cpus: self.cpu_limit,
                pids: Some(PIDS_LIMIT),
                build_images: ran
                    .services
                    .iter()
                    .filter(|(_, s)| s.build.is_some())
                    .map(|(svc, _)| {
                        (
                            svc.clone(),
                            format!(
                                "devstack-build-{}",
                                secret_name(&self.secret, &[&self.member, &mirror.project, svc])
                            ),
                        )
                    })
                    .collect(),
            };
            let text = devstack_compose::node_overlay(&ran, &mirror.project, &self.slot, &options)
                .map_err(|e| RunError::Config(format!("compose group `{name}`: {e}")))?;
            let overlay = compose_dir.join(format!("{name}.yml"));
            std::fs::write(&overlay, text).map_err(|e| RunError::Io(overlay.clone(), e))?;
            let project = self.project(&stack, &name);
            self.record(&project)?;
            if fresh {
                phase(&format!(
                    "infrastructure: {name}: its data goes (a fresh start)"
                ));
                remove_project(&project, true);
            }
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
            for n in project_networks(&g.project, &g.networks) {
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
            crate::egress::apply(
                &self.state(),
                &owner_tag(&self.secret),
                &self.id,
                &bridges(&networks),
                self.network,
            )
            .map_err(RunError::Docker)?;
        }

        // The lane's daemon starts before its containers do (#164): it
        // reads them as created, and waits for each group as the agent
        // brings it up after answering ([`InfraUp::run`]).
        let (infra, infra_ports) = initial_infra(&groups, self.port_offset);
        let io = |p: &str| {
            let p = src.join(p);
            move |e| RunError::Io(p, e)
        };
        let infra_json = serde_json::to_vec_pretty(&infra).expect("infra serialises");
        safefs::write_beneath(&src, Path::new(crate::laneinfra::LANE_FILE), 0o644, |f| {
            std::io::Write::write_all(f, &infra_json)
        })
        .map_err(io(crate::laneinfra::LANE_FILE))?;
        // The agent's own copy, beside the mirror where the lane cannot
        // write: the projects it started, their groups' states, and what it
        // refreshes the daemon's file from (`laneinfra`).
        crate::laneinfra::write_agent_file(&self.dir, &infra_json)
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
        let owner = format!("devstack.agent={}", owner_tag(&self.secret));
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
            "--label",
            &owner,
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
        Ok(Started {
            addr: DaemonAddr {
                addr,
                token,
                infra_ports,
            },
            infra: InfraUp {
                groups: groups.into_iter().map(|(n, g, _)| (n, g)).collect(),
            },
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

    /// Its container, then every Compose project it started here (by
    /// Compose's labels, not by its files: a stack file the lane changed or
    /// broke takes nothing along), the newest first.
    fn take_down(&self, agent: Option<&str>, volumes: bool) {
        let _ = docker(&["rm", "-f", &self.container()]);
        let own = self.own_network();
        if let Some(me) = agent {
            let _ = docker(&["network", "disconnect", "-f", &own, me]);
        }
        let _ = docker(&["network", "rm", &own]);
        crate::egress::remove(&self.state(), &owner_tag(&self.secret), &self.id);
        let stack = self.stack().ok();
        for p in self.projects(stack.as_ref()).iter().rev() {
            // The agent leaves the lane's networks first: Docker does not
            // remove a network something is still on.
            if let Some(me) = agent {
                for n in project_networks(&p.project, &p.networks) {
                    let _ = docker(&["network", "disconnect", "-f", &n, me]);
                }
            }
            remove_project(p, volumes);
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
fn project_networks(project: &str, own: &[String]) -> Vec<String> {
    let mut out = by_label(&["network", "ls", "--format", "{{.Name}}"], project);
    for n in own {
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
    fn the_daemon_reads_its_infrastructure_as_created_before_it_runs() {
        let model = devstack_compose::parse_config(
            r#"{"name":"shop","services":{
                "db":{"image":"postgres","environment":{"POSTGRES_DB":"app"},
                      "ports":[{"target":5432,"published":"15432"}]},
                "cache":{"image":"redis","ports":[{"target":6379}]},
                "seed":{"image":"busybox","ports":[{"target":80,"published":"8080"}]}}}"#,
        )
        .unwrap();
        let g = Group {
            name: "app".into(),
            project: "shop-ab12".into(),
            files: Vec::new(),
            env_files: Vec::new(),
            networks: Vec::new(),
            services: vec!["db".into(), "cache".into()],
            dir: PathBuf::from("/compose"),
        };
        let (infra, ports) = initial_infra(&[("app".into(), g, model)], 100);
        let app = &infra["groups"]["app"];
        assert_eq!(app["state"], crate::laneinfra::CREATED);
        assert_eq!(app["project"], "shop-ab12");
        let db = &app["services"]["db"];
        assert_eq!(db["state"], "created");
        assert_eq!(db["environment"]["POSTGRES_DB"], "app");
        assert_eq!(db["ports"]["5432"], "db:5432");
        assert_eq!(db["published"]["5432"], 15532, "moved by the lane's offset");
        assert_eq!(app["services"]["cache"]["ports"]["6379"], "cache:6379");
        // Parked: an environment source only, without a state, its port not
        // forwarded.
        assert_eq!(app["services"]["seed"]["state"], "");
        assert_eq!(ports, [15532], "only what the group starts is forwarded");
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
