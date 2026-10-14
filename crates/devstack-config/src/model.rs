//! The stack file model, schema version 1 (ADR-0017, plan §4).
//!
//! Every table rejects unknown keys: a typo is an error, never a silently
//! ignored setting. Services, commands and Compose groups are keyed tables (no
//! anonymous list entries), so a later version can layer stack files by key.

use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
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
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StackMeta {
    /// Display name; also the prefix of the daemon's state.
    pub name: String,
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
    /// Groups that must be healthy first.
    #[serde(default)]
    pub after: Vec<String>,
    /// Per started service: paths whose change restarts that container.
    #[serde(default)]
    pub restart_on: BTreeMap<String, Vec<String>>,
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default)]
    pub kind: Kind,
    /// Command run in the lane's snapshot with the normal output directories (ADR-0002).
    pub build: Option<String>,
    /// Directory, relative to the repository, copied into the artifact after a successful build.
    pub output: Option<String>,
    /// Command that starts the service.
    pub run: String,
    /// Working directory relative to the repository.
    pub cwd: Option<String>,
    /// Paths whose change starts a candidate build (ADR-0003).
    #[serde(default)]
    pub watch: Vec<String>,
    /// Paths whose change restarts a dev server.
    #[serde(default)]
    pub restart_on: Vec<String>,
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

// ------------------------------------------------------------------ durations

/// `500ms`, `30s`, `5m`, `1h`; a bare number is seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid duration `{s}` (expected e.g. `30s`, `5m`, `500ms`)"))?;
    let d = match unit.trim() {
        "ms" => Duration::from_millis(n),
        "" | "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 3600),
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
