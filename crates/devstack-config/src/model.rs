//! The stack file model, schema version 1 (ADR-0017, plan §4).
//!
//! Every table rejects unknown keys: a typo is an error, never a silently
//! ignored setting. Services, commands and Compose groups are keyed tables (no
//! anonymous list entries), so a later version can layer stack files by key.

use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    /// Schema version of this file; `1`.
    pub version: u32,
    pub stack: StackMeta,
    /// Compose projects of the infra plane, keyed by group name (ADR-0004).
    #[serde(default)]
    pub compose: BTreeMap<String, ComposeGroup>,
    /// Native processes managed by devstack, keyed by service name.
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    /// Operations users and agents can run, keyed by command name.
    #[serde(default)]
    pub commands: BTreeMap<String, Command>,
    /// Views over services, Compose services and commands (plan §4.4a). Labels,
    /// not parents: no member depends on another, a member may be in several.
    #[serde(default)]
    pub groups: BTreeMap<String, Group>,
    /// Other working trees run as lanes (plan §3.5).
    #[serde(default)]
    pub lanes: Lanes,
    /// How a tree runs for a purpose (an acceptance run, ADR-0041), chosen
    /// when it starts: `devstack up --profile <name>`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// What devstack writes for the coding agents that work in the
    /// repository.
    #[serde(default)]
    pub agents: Agents,
    /// The profile this stack runs with, once applied ([`Stack::with_profile`]).
    #[serde(skip)]
    #[schemars(skip)]
    pub profile: Option<Active>,
}

/// `[agents]`: what devstack writes for the coding agents that work in the
/// repository.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Agents {
    /// The folders `devstack skills install` writes the generated skills to,
    /// relative to the stack file: `.claude/skills` (Claude Code),
    /// `.agents/skills` (Codex, Gemini CLI and most others), or both. Agents
    /// that read both find a skill twice. Not set: `.claude/skills`.
    #[serde(default)]
    pub skills: Option<Vec<String>>,
}

impl Agents {
    /// Where the generated skills are written when `skills` is not set.
    pub const DEFAULT_SKILLS: &str = ".claude/skills";

    /// The skill folders, `skills` or the default.
    pub fn skill_dirs(&self) -> Vec<String> {
        self.skills
            .clone()
            .unwrap_or_else(|| vec![Self::DEFAULT_SKILLS.to_string()])
    }
}

/// `[profiles.<name>]` (ADR-0041): an overlay that changes how the stack
/// runs, never what it is.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// What it is for, shown where profiles are offered.
    #[serde(default)]
    pub description: Option<String>,
    /// Background workers in a lane (as `--workers`); in the main tree they
    /// run anyway.
    #[serde(default)]
    pub workers: Option<bool>,
    /// `fresh`: the tree's own Compose volumes go before its containers
    /// start, so its seed jobs fill a new database.
    #[serde(default)]
    pub data: Option<DataMode>,
    /// Variables for every service and command of the tree; the usual
    /// placeholders (`{service.web.url}`, `{compose.app.db.5432}`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The services that start, instead of each service's own `autostart`.
    #[serde(default)]
    pub autostart: Option<Vec<String>>,
    /// How single services run with this profile.
    #[serde(default)]
    pub services: BTreeMap<String, ServiceProfile>,
    /// A `[commands]` entry that is the profile's purpose (the suite):
    /// `devstack up --profile <name> --run` runs it once the tree is ready.
    #[serde(default)]
    pub command: Option<String>,
    /// What a lane with this profile needs at least, over `[lanes.need]`
    /// (ADR-0049 point 5).
    #[serde(default)]
    pub need: Need,
}

/// `fresh` or `keep` (the default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DataMode {
    #[default]
    Keep,
    Fresh,
}

/// What a profile changes of one service: the fields that say how it runs.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceProfile {
    pub kind: Option<Kind>,
    pub build: Option<String>,
    pub output: Option<String>,
    pub run: Option<String>,
    pub spa: Option<bool>,
    pub cwd: Option<String>,
    pub health: Option<Health>,
    /// Added to the service's `env`, over the profile's.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// The profile a stack runs with, applied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Active {
    pub name: String,
    pub workers: Option<bool>,
    pub data: DataMode,
    pub env: BTreeMap<String, String>,
    pub command: Option<String>,
}

impl Stack {
    /// The stack as profile `name` runs it (ADR-0041), checked like a stack
    /// file; the issues name the profile's keys.
    /// What a lane needs at least with `profile` (ADR-0049 point 5):
    /// `[lanes.need]`, raised by the profile's.
    pub fn lane_need(&self, profile: Option<&str>) -> Need {
        match profile.and_then(|p| self.profiles.get(p)) {
            Some(p) => self.lanes.need.max(&p.need),
            None => self.lanes.need.clone(),
        }
    }

    pub fn with_profile(&self, name: &str) -> Result<Stack, Vec<crate::Issue>> {
        let s = self.apply_profile(name)?;
        let issues = crate::validate::validate(&s);
        if issues.is_empty() {
            Ok(s)
        } else {
            Err(issues
                .into_iter()
                .map(|i| crate::Issue {
                    path: format!("profiles.{name} ({})", i.path),
                    message: i.message,
                })
                .collect())
        }
    }

    /// [`Self::with_profile`] without the check; unknown names are issues.
    /// The applied stack keeps its `profiles`, to tell what else there is.
    pub(crate) fn apply_profile(&self, name: &str) -> Result<Stack, Vec<crate::Issue>> {
        let issue = |path: String, message: String| crate::Issue { path, message };
        let Some(p) = self.profiles.get(name) else {
            let known: Vec<&str> = self.profiles.keys().map(String::as_str).collect();
            return Err(vec![issue(
                "profiles".into(),
                if known.is_empty() {
                    format!("no profile `{name}`: the stack file has none")
                } else {
                    format!("no profile `{name}` (there are {})", known.join(", "))
                },
            )]);
        };
        let mut s = self.clone();
        let mut issues = Vec::new();
        for svc in s.services.values_mut() {
            for (k, v) in &p.env {
                svc.env.insert(k.clone(), v.clone());
            }
        }
        for (n, o) in &p.services {
            let Some(svc) = s.services.get_mut(n) else {
                issues.push(issue(
                    format!("profiles.{name}.services.{n}"),
                    format!("no service `{n}` in `[services]`"),
                ));
                continue;
            };
            if let Some(k) = o.kind
                && k != svc.kind
            {
                svc.kind = k;
                // What only the old kind takes goes (a dev server's `rebuild`
                // under a `static` profile, say): the profile cannot unset a
                // key, and the stack it makes must be valid. What the profile
                // gives itself is set below.
                if k == Kind::Static {
                    svc.run = String::new();
                }
                if k != Kind::Devserver {
                    svc.rebuild = None;
                    svc.restart_on.clear();
                }
                if matches!(k, Kind::Devserver | Kind::Tool) {
                    svc.build = None;
                    svc.output = None;
                }
                if k != Kind::Static {
                    svc.spa = None;
                }
                if k != Kind::Service {
                    svc.hot_reload = None;
                }
                if k != Kind::Job {
                    svc.timeout = None;
                }
                if matches!(k, Kind::Job | Kind::Tool) {
                    svc.health = None;
                    svc.proxy = None;
                }
            }
            if o.build.is_some() {
                svc.build = o.build.clone();
            }
            if o.output.is_some() {
                svc.output = o.output.clone();
            }
            if let Some(r) = &o.run {
                svc.run = r.clone();
            }
            if o.spa.is_some() {
                svc.spa = o.spa;
            }
            if o.cwd.is_some() {
                svc.cwd = o.cwd.clone();
            }
            if o.health.is_some() {
                svc.health = o.health.clone();
            }
            for (k, v) in &o.env {
                svc.env.insert(k.clone(), v.clone());
            }
        }
        if let Some(list) = &p.autostart {
            for (i, n) in list.iter().enumerate() {
                if !s.services.contains_key(n) {
                    issues.push(issue(
                        format!("profiles.{name}.autostart[{i}]"),
                        format!("no service `{n}` in `[services]`"),
                    ));
                }
            }
            for (n, svc) in s.services.iter_mut() {
                svc.autostart = list.contains(n);
            }
        }
        if let Some(c) = &p.command
            && !s.commands.contains_key(c)
        {
            issues.push(issue(
                format!("profiles.{name}.command"),
                format!("no command `{c}` in `[commands]`"),
            ));
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        s.profile = Some(Active {
            name: name.to_string(),
            workers: p.workers,
            data: p.data.unwrap_or_default(),
            env: p.env.clone(),
            command: p.command.clone(),
        });
        Ok(s)
    }
}

/// Lanes: another working tree (a `git worktree`, an agent's checkout) runs the
/// stack's services beside the main one, on its own ports, with its own
/// containers (plan §3.5, ADR-0030), on this machine or a node of the pool
/// (ADR-0025).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lanes {
    /// Each lane's services listen on their port plus this times the lane's
    /// number (1, 2, ...): with 100, `api` on 5000 is 5100 in the first lane.
    #[serde(default = "default_port_step")]
    pub port_step: u16,
    /// The image a lane runs in on another machine (ADR-0028): a Linux image
    /// with the stack's toolchains, without devstack. With `dockerfile` too,
    /// `image` wins.
    #[serde(default)]
    pub image: Option<String>,
    /// A Dockerfile in the repository the node builds the lane image from
    /// instead (ADR-0028 point 6), relative to the stack file.
    #[serde(default)]
    pub dockerfile: Option<PathBuf>,
    /// Files git ignores that a lane on another machine needs all the same
    /// (a local TLS certificate, say): globs relative to the repository,
    /// taken from the lane's worktree or, where it has none, from the main
    /// tree. Keep them narrow: they are looked for at every sync.
    #[serde(default)]
    pub sync: Vec<String>,
    /// Whom a lane on a node is shared with unless its owner says otherwise
    /// (ADR-0035): member ids or tags of the pool, `*` for every member;
    /// none (the default): nobody.
    #[serde(default)]
    pub share: Vec<String>,
    /// Whether a lane on a node keeps its data (its volumes, its mirror,
    /// its builds) when it stops, for the next start there (ADR-0038).
    /// `false`: every start there is a fresh one. The node decides how long
    /// it keeps them.
    #[serde(default = "yes")]
    pub keep_data: bool,
    /// What a lane needs at least (ADR-0049 point 5): a floor on what the
    /// pool measured or this machine estimates, never an override.
    #[serde(default)]
    pub need: Need,
}

impl Default for Lanes {
    fn default() -> Self {
        Self {
            port_step: default_port_step(),
            image: None,
            dockerfile: None,
            sync: Vec::new(),
            share: Vec::new(),
            keep_data: true,
            need: Need::default(),
        }
    }
}

/// `[lanes.need]`, `[profiles.<name>.need]` (ADR-0049 point 5): what a lane
/// needs at least. Sizes are `512M`, `6G`, `25GiB`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Need {
    /// CPU cores.
    #[serde(default)]
    pub cpus: Option<f64>,
    /// Memory.
    #[serde(default, deserialize_with = "opt_size")]
    #[schemars(with = "Option<String>")]
    pub memory: Option<u64>,
    /// Disk, in all: the lane's images, its source and dependencies, its
    /// builds and its volumes.
    #[serde(default, deserialize_with = "opt_size")]
    #[schemars(with = "Option<String>")]
    pub disk: Option<u64>,
    /// GPUs: a lane in the pool goes only to a node that offers them.
    #[serde(default)]
    pub gpus: Option<GpuNeed>,
}

/// `gpus = { count = 1, memory = "16G" }`
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GpuNeed {
    /// How many; 1 if not given.
    #[serde(default = "one")]
    pub count: u32,
    /// The memory each must have, at least.
    #[serde(default, deserialize_with = "opt_size")]
    #[schemars(with = "Option<String>")]
    pub memory: Option<u64>,
}

fn one() -> u32 {
    1
}

impl Need {
    /// Each part the larger of the two.
    pub fn max(&self, o: &Need) -> Need {
        fn most<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(x), Some(y)) => Some(if y > x { y } else { x }),
                (x, y) => x.or(y),
            }
        }
        Need {
            cpus: most(self.cpus, o.cpus),
            memory: most(self.memory, o.memory),
            disk: most(self.disk, o.disk),
            gpus: match (&self.gpus, &o.gpus) {
                (Some(a), Some(b)) => Some(GpuNeed {
                    count: a.count.max(b.count),
                    memory: most(a.memory, b.memory),
                }),
                (a, b) => a.clone().or_else(|| b.clone()),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Need::default()
    }
}

fn default_port_step() -> u16 {
    100
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// Display name; defaults to the group's key.
    pub label: Option<String>,
    /// `<service>`, `compose.<group>.<service>` or `command.<name>`.
    pub members: Vec<String>,
    /// Sort position in the dashboard; groups without one follow alphabetically.
    pub order: Option<i32>,
}

impl Stack {
    /// The Compose service a name stands for, `(group, service)`, as the
    /// CLI takes it (#128): `<group>.<service>`, the group member's form
    /// `compose.<group>.<service>`, or a bare `<service>` that exactly one
    /// Compose group lists in its `services`. The dotted forms need the
    /// Compose group only (the container routes tell a service the group
    /// does not start). A service or group of the stack file by that name
    /// comes first: callers look for those before.
    pub fn compose_service(&self, name: &str) -> Option<(String, String)> {
        let dotted = name.strip_prefix("compose.").unwrap_or(name);
        if let Some((group, service)) = dotted.split_once('.') {
            return (!service.is_empty()
                && !service.contains('.')
                && self.compose.contains_key(group))
            .then(|| (group.to_string(), service.to_string()));
        }
        if name.starts_with("compose.") {
            return None;
        }
        let mut groups = self
            .compose
            .iter()
            .filter(|(_, g)| g.services.iter().any(|s| s == name));
        let (group, _) = groups.next()?;
        groups
            .next()
            .is_none()
            .then(|| (group.clone(), name.to_string()))
    }

    /// The Compose services group `group` has as members
    /// (`compose.<group>.<service>`), in member order.
    pub fn group_containers(&self, group: &str) -> Vec<(String, String)> {
        self.groups
            .get(group)
            .map(|g| {
                g.parsed_members()
                    .into_iter()
                    .filter_map(|m| match m {
                        Ok(Member::ComposeService { group, service }) => {
                            Some((group.to_string(), service.to_string()))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A group member, resolved from its string form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member<'a> {
    Service(&'a str),
    ComposeService { group: &'a str, service: &'a str },
    Command(&'a str),
}

impl Group {
    /// Parse every member; `Err` carries the members that are not well-formed.
    pub fn parsed_members(&self) -> Vec<Result<Member<'_>, &str>> {
        self.members
            .iter()
            .map(|m| {
                if let Some(rest) = m.strip_prefix("compose.") {
                    match rest.split_once('.') {
                        Some((g, s)) if !g.is_empty() && !s.is_empty() && !s.contains('.') => {
                            Ok(Member::ComposeService {
                                group: g,
                                service: s,
                            })
                        }
                        _ => Err(m.as_str()),
                    }
                } else if let Some(c) = m.strip_prefix("command.") {
                    if c.is_empty() {
                        Err(m.as_str())
                    } else {
                        Ok(Member::Command(c))
                    }
                } else if m.is_empty() || m.contains('.') {
                    Err(m.as_str())
                } else {
                    Ok(Member::Service(m))
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackMeta {
    /// Display name; also the prefix of the daemon's state.
    pub name: String,
    /// Oldest devstack that can run this file, e.g. `"0.1.0"`. Checked before
    /// anything else, so an old devstack asks to be updated instead of failing
    /// on a key it does not know.
    pub requires: Option<String>,
    /// The devstack this file was last brought up to date with (what changed
    /// read, the file held against its reference), e.g. `"0.20.0"`. While it
    /// names an older one than is installed, the dashboard offers a coding
    /// agent the upgrade. Not the agent skills' version: those are refreshed
    /// on their own.
    pub reviewed: Option<String>,
    /// Env files read for `${VAR}` in every service and command, relative to the
    /// repository; missing files are skipped. A service's Compose group files and
    /// the process environment win over them.
    #[serde(default)]
    pub env_files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposeGroup {
    /// Compose project name. Fixed, never derived from a path.
    pub project: String,
    /// Deployment Compose files first, then the dev overlay.
    pub files: Vec<String>,
    /// Env files; later files win.
    #[serde(default)]
    pub env_files: Vec<String>,
    /// External networks created before `up`.
    #[serde(default)]
    pub networks: Vec<String>,
    /// Services started (`--no-deps`); every other service of the files stays down.
    pub services: Vec<String>,
    /// Of `services`, those a lane uses from the main tree instead of
    /// starting its own (ADR-0030): only for what holds nothing that belongs
    /// to one lane (a telemetry dashboard, a fake SMTP server). Reached by
    /// their published ports; containers of a lane do not see them by name.
    #[serde(default)]
    pub shared: Vec<String>,
    /// Of `services`, those whose published TCP ports are devstack's in the
    /// main tree (ADR-0048): held with a forward on the fixed port, which
    /// `promote --lane` points at the lane's. For what the developer reaches
    /// directly from the host (a UI, the API the browser calls).
    #[serde(default)]
    pub main_ports: Vec<String>,
    /// Groups that must be healthy first.
    #[serde(default)]
    pub after: Vec<String>,
    /// Per started service: paths whose change restarts that container.
    #[serde(default)]
    pub restart_on: BTreeMap<String, Vec<String>>,
    /// Per started service: the address the dashboard links to, e.g.
    /// `https://localhost:9443/ui`; `""` for no link (a database). Without an
    /// entry every published port links to `http://localhost:<port>`.
    #[serde(default)]
    pub urls: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Long-running, built into an artifact, health-checked.
    #[default]
    Service,
    /// Runs to completion before its dependants start (e.g. a schema migrator).
    Job,
    /// Builds and serves in one process (a hot-reloading dev server).
    Devserver,
    /// Long-running and interactive, no health check (e.g. a test UI).
    Tool,
    /// A frontend's production bundle, built into an artifact and served as
    /// files by devstack (plan §3.4): for end-to-end runs on a pinned build.
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Windows,
    Linux,
    Macos,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        }
    }
}

/// A dev server's rebuilds, read from its output (ADR-0042).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Rebuild {
    /// `auto`, `none`, or a known tool: `angular`, `vite`, `webpack`,
    /// `next`, `dotnet-watch`, `tsc`.
    Named(String),
    /// The tool's own lines, as regular expressions.
    Lines(RebuildLines),
}

/// A tool's lines for a rebuild (ADR-0042 point 3).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RebuildLines {
    /// A rebuild starts; optional (some tools say only when one is done).
    #[serde(default)]
    pub start: Option<String>,
    /// A rebuild is done; a capture group `ms` or `s` gives its time.
    pub ok: String,
    /// A rebuild failed.
    pub failed: String,
}

/// The tools [`Rebuild::Named`] knows, besides `auto` and `none`.
pub const REBUILD_TOOLS: &[&str] = &["angular", "vite", "webpack", "next", "dotnet-watch", "tsc"];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default)]
    pub kind: Kind,
    /// Command run in the lane's snapshot with the normal output directories (ADR-0002).
    pub build: Option<String>,
    /// Directory, relative to the repository, copied into the artifact after a successful build.
    pub output: Option<String>,
    /// Command that starts the service (not for `kind = "static"`: devstack
    /// serves the artifact itself).
    #[serde(default)]
    pub run: String,
    /// `kind = "static"`: answer paths without a file extension with
    /// `index.html` (client-side routes of a single-page app). Default `true`.
    pub spa: Option<bool>,
    /// Working directory relative to the repository.
    pub cwd: Option<String>,
    /// Paths whose change starts a candidate build (ADR-0003).
    #[serde(default)]
    pub watch: Vec<String>,
    /// Paths whose change restarts a dev server.
    #[serde(default)]
    pub restart_on: Vec<String>,
    /// How devstack reads a dev server's rebuilds from its output
    /// (ADR-0042): absent or `"auto"`, the tool recognised from its lines;
    /// a known tool by name; `"none"`; or its own lines.
    #[serde(default)]
    pub rebuild: Option<Rebuild>,
    /// Fixed port of the main slot.
    pub port: Option<u16>,
    pub health: Option<Health>,
    /// Services, or `compose.<group>.<service>`, that must be ready first.
    #[serde(default)]
    pub after: Vec<String>,
    /// `false`: started on demand only.
    #[serde(default = "yes")]
    pub autostart: bool,
    /// Restrict to these platforms; empty = all.
    #[serde(default)]
    pub platforms: Vec<Platform>,
    /// `false`: runs in the main working tree only, never in a lane
    /// (ADR-0024). For what may run once per machine (a tray app, something
    /// that holds a global lock); the main tree's instance serves every lane
    /// and keeps its workers while a lane holds the main slot.
    #[serde(default = "yes")]
    pub lanes: bool,
    /// Compose service whose environment is the base (ADR-0014).
    pub compose: Option<ComposeRef>,
    /// Compose services whose `name:port` is rewritten to the published host
    /// address (ADR-0014): `<service>` in the service's own group, or
    /// `<group>.<service>` in another one.
    #[serde(default)]
    pub rewrite_hosts: Vec<String>,
    /// Environment; wins over the Compose environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Worker switches (ADR-0008).
    pub workers: Option<Workers>,
    /// Directories created before start.
    #[serde(default)]
    pub ensure_dirs: Vec<String>,
    pub promote: Option<Promote>,
    /// Serve the fixed port through devstack's proxy (ADR-0010): instances run
    /// on internal ports and a new one takes over without dropped requests.
    /// Default on for long-running services with a port; `false` restarts in
    /// place on the fixed port. For a dev server: its fixed port forwarded
    /// to its instance (ADR-0043), default on where `run` takes `{port}`.
    pub proxy: Option<bool>,
    /// The command for hot-reload mode (plan §3.4a): `devstack hot-reload
    /// <service> on` runs it from the source tree instead of the build, until
    /// switched off. Built services only.
    pub hot_reload: Option<HotReload>,
    /// Installs what a copy of the source lacks (plan §3.5): a snapshot holds
    /// tracked and untracked non-ignored files only, so `node_modules` and the
    /// like are not in it. Runs in the snapshot before the service is built
    /// there, and in a lane's copy before the service starts from it.
    pub prepare: Option<Prepare>,
    /// `kind = "job"`: how long it may run before it counts as failed and is
    /// stopped, e.g. `"10m"`. Default 30 minutes.
    #[schemars(with = "Option<String>")]
    #[serde(default, deserialize_with = "opt_duration")]
    pub timeout: Option<Duration>,
}

/// `prepare` of a service: a command run in a copy of the source.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    /// Command, e.g. `npm ci`.
    pub run: String,
    /// Working directory relative to the repository; the service's `cwd` by default.
    pub cwd: Option<String>,
    /// Files whose change runs it again, e.g. `["web/package-lock.json"]`;
    /// empty: once per copy.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Directories it installs that a lane's worktree gets linked to, e.g.
    /// `["web/node_modules"]` (ADR-0044): the worktree's git hooks and
    /// editor find them there. Relative to the repository; only where the
    /// worktree has nothing of that name, and removed when the lane stops.
    #[serde(default)]
    pub link: Vec<String>,
}

/// Hot-reload mode of a built service: the toolchain's own watcher, from
/// the source tree, behind the same port (plan §3.4a).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HotReload {
    /// Command started instead of `run`, e.g. `dotnet watch run --non-interactive`.
    pub run: String,
    /// Working directory relative to the repository; the service's `cwd` by default.
    pub cwd: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// Path of an HTTP health endpoint on the service's port.
    pub http: String,
    /// How long to wait for the service to become healthy, e.g. `60s`.
    #[schemars(with = "String")]
    #[serde(deserialize_with = "duration")]
    pub timeout: Duration,
    /// Accept a self-signed certificate (dev servers with HTTPS).
    #[serde(default)]
    pub insecure_tls: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposeRef {
    pub group: String,
    pub service: String,
    /// `false`: the Compose service is a reference only. Its environment is not
    /// taken over (it describes a deployment, not this service, ADR-0014), and
    /// `devstack check --compose` lists its variables the stack file does not
    /// set, so a new one is noticed instead of silently missing.
    #[serde(default = "yes")]
    pub inherit: bool,
    /// With `inherit = false`: variables left out on purpose (globs, `*`).
    #[serde(default)]
    pub ignore: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Workers {
    /// Variables that switch the service's background workers; `{is_main}` resolves per slot.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Promote {
    /// Promote automatically after a healthy candidate (default true).
    #[serde(default = "yes")]
    pub auto: bool,
    /// A candidate that changed one of these paths waits for an explicit promote (ADR-0013).
    #[serde(default)]
    pub hold_if_changed: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub description: String,
    /// Confirmation text; the command does not run unless it is acknowledged.
    pub confirm: Option<String>,
    /// Services that must be healthy for the command to be enabled.
    #[serde(default)]
    pub requires: Vec<String>,
    pub steps: Vec<Step>,
}

/// One step of a command. Exactly one action key is set; see [`Step::action`].
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Shell command line.
    pub exec: Option<String>,
    /// Working directory of `exec`, relative to the repository.
    pub cwd: Option<String>,
    /// `METHOD url`, e.g. `POST {service.api.url}/seed`.
    pub http: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Request body of an `http` step (placeholders and `${VAR}` expanded).
    pub body: Option<String>,
    #[schemars(with = "Option<String>")]
    #[serde(default, deserialize_with = "opt_duration")]
    pub timeout: Option<Duration>,
    pub start: Option<String>,
    pub stop: Option<String>,
    pub restart: Option<String>,
    /// Run a `job` service to completion.
    pub run: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepAction<'a> {
    Exec {
        command: &'a str,
        cwd: Option<&'a str>,
    },
    Http {
        method: &'a str,
        url: &'a str,
    },
    Start(&'a str),
    Stop(&'a str),
    Restart(&'a str),
    Run(&'a str),
}

impl Step {
    /// The step's single action, or the names of all action keys that were set
    /// when that is not exactly one.
    pub fn action(&self) -> Result<StepAction<'_>, Vec<&'static str>> {
        let mut set: Vec<&'static str> = Vec::new();
        let mut action = None;
        if let Some(c) = &self.exec {
            set.push("exec");
            action = Some(StepAction::Exec {
                command: c,
                cwd: self.cwd.as_deref(),
            });
        }
        if let Some(h) = &self.http {
            set.push("http");
            let (method, url) = h.trim().split_once(char::is_whitespace).unwrap_or(("", h));
            action = Some(StepAction::Http {
                method,
                url: url.trim(),
            });
        }
        if let Some(v) = &self.start {
            set.push("start");
            action = Some(StepAction::Start(v));
        }
        if let Some(v) = &self.stop {
            set.push("stop");
            action = Some(StepAction::Stop(v));
        }
        if let Some(v) = &self.restart {
            set.push("restart");
            action = Some(StepAction::Restart(v));
        }
        if let Some(v) = &self.run {
            set.push("run");
            action = Some(StepAction::Run(v));
        }
        match (set.len(), action) {
            (1, Some(a)) => Ok(a),
            _ => Err(set),
        }
    }
}

// ------------------------------------------------------------------ sizes

/// `512M`, `6G`, `6GiB`, `1.5T`; `k`/`M`/`G`/`T` with or without `B`/`iB`
/// are powers of 1024; a bare number is bytes.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let bad = || format!("invalid size `{s}` (expected e.g. `512M`, `6G`, `25GiB`)");
    let n: f64 = num.parse().map_err(|_| bad())?;
    let unit = unit.trim().to_ascii_lowercase();
    let unit = unit
        .strip_suffix("ib")
        .or_else(|| unit.strip_suffix('b'))
        .unwrap_or(&unit);
    let factor: u64 = match unit {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return Err(bad()),
    };
    let v = n * factor as f64;
    if !v.is_finite() || v < 0.0 || v > u64::MAX as f64 {
        return Err(bad());
    }
    Ok(v as u64)
}

fn opt_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Size {
        Text(String),
        Bytes(u64),
    }
    Ok(match Option::<Size>::deserialize(d)? {
        None => None,
        Some(Size::Bytes(b)) => Some(b),
        Some(Size::Text(t)) => Some(parse_size(&t).map_err(serde::de::Error::custom)?),
    })
}

// ------------------------------------------------------------------ durations

/// `500ms`, `30s`, `5m`, `1h`; a bare number is seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid duration `{s}` (expected e.g. `30s`, `5m`, `500ms`)"))?;
    let too_long = || format!("duration `{s}` is too long");
    let d = match unit.trim() {
        "ms" => Duration::from_millis(n),
        "" | "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n.checked_mul(60).ok_or_else(too_long)?),
        "h" => Duration::from_secs(n.checked_mul(3600).ok_or_else(too_long)?),
        other => {
            return Err(format!(
                "invalid duration unit `{other}` in `{s}` (use ms, s, m or h)"
            ));
        }
    };
    Ok(d)
}

fn duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let s = String::deserialize(d)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

fn opt_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let s = Option::<String>::deserialize(d)?;
    s.map(|s| parse_duration(&s).map_err(serde::de::Error::custom))
        .transpose()
}

impl Service {
    /// `https` when the health check speaks TLS (a dev server with a
    /// certificate), otherwise `http`: how a browser or a command reaches the
    /// service on its fixed port.
    pub fn scheme(&self) -> &'static str {
        if self.health.as_ref().is_some_and(|h| h.insecure_tls) {
            "https"
        } else {
            "http"
        }
    }

    /// Behind devstack's proxy (ADR-0010).
    pub fn proxied(&self) -> bool {
        matches!(self.kind, Kind::Service | Kind::Static)
            && self.port.is_some()
            && self.proxy.unwrap_or(true)
    }

    /// A dev server whose fixed port devstack holds and forwards to its
    /// instance (ADR-0043): one with a `port` whose `run` takes `{port}`,
    /// unless `proxy = false`.
    pub fn fronted(&self) -> bool {
        self.kind == Kind::Devserver
            && self.port.is_some()
            && self.proxy.unwrap_or(true)
            && self.run_takes_port()
    }

    /// `run` has the placeholder `{port}` (not the literal text `{{port}}`).
    pub fn run_takes_port(&self) -> bool {
        use crate::interpolate::{Placeholder, parse, placeholders};
        parse(&self.run).is_ok_and(|segs| {
            placeholders(&segs)
                .iter()
                .any(|p| matches!(p, Placeholder::Port))
        })
    }
}
