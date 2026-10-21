//! The infra plane (ADR-0004): devstack drives the project's own Compose files
//! and never manages containers itself. Every call names the project and the
//! files explicitly; `up` starts exactly the selected services with
//! `--no-deps`, ordering is devstack's job.
//!
//! Formats verified against Docker Compose v5.6: `config --format json` gives
//! `published` as a string and omits it for dynamically assigned ports;
//! `ps --format json` prints one JSON object per line, with `Publishers`.

pub mod engine;
use devstack_config::model::ComposeGroup;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("cannot run `{program}`: {source} (is Docker installed and on PATH?)")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("`{command}` failed ({status}):\n{output}")]
    Failed {
        command: String,
        status: String,
        output: String,
    },
    #[error("`{command}` did not finish within {secs} s (is Docker answering?)")]
    Timeout { command: String, secs: u64 },
    #[error("cannot read the output of `{command}`: {message}")]
    Parse { command: String, message: String },
    /// Ended by its caller before it finished ([`output_until`]).
    #[error("`{command}` was ended: the lane stops or starts afresh")]
    Stopped { command: String },
}

pub type Result<T> = std::result::Result<T, ComposeError>;

/// One Compose group of the stack file, with paths resolved against the repository.
#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub project: String,
    pub files: Vec<PathBuf>,
    pub env_files: Vec<PathBuf>,
    pub networks: Vec<String>,
    pub services: Vec<String>,
    /// Working directory for every call: the repository root.
    pub dir: PathBuf,
}

impl Group {
    pub fn from_config(name: &str, g: &ComposeGroup, repo: &Path) -> Self {
        Self {
            name: name.into(),
            project: g.project.clone(),
            files: g.files.iter().map(|f| repo.join(f)).collect(),
            env_files: g.env_files.iter().map(|f| repo.join(f)).collect(),
            networks: g.networks.clone(),
            services: g.services.clone(),
            dir: repo.to_path_buf(),
        }
    }

    /// `compose -p <project> -f … --env-file …` (env files that do not exist are skipped,
    /// so an untracked `.env` may be absent).
    pub fn base_args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["compose".into(), "-p".into(), self.project.clone().into()];
        for f in &self.files {
            a.push("-f".into());
            a.push(f.into());
        }
        for e in self.env_files.iter().filter(|e| e.exists()) {
            a.push("--env-file".into());
            a.push(e.into());
        }
        a
    }
}

// ------------------------------------------------------------------- models

/// A field Compose may write as JSON `null` (it serialises empty Go slices
/// and maps that way): read as its default.
fn nullable<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The parts of `docker compose config --format json` devstack reads.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Model {
    #[serde(default, deserialize_with = "nullable")]
    pub services: BTreeMap<String, ServiceModel>,
    #[serde(default, deserialize_with = "nullable")]
    pub networks: BTreeMap<String, Option<NamedModel>>,
    #[serde(default, deserialize_with = "nullable")]
    pub volumes: BTreeMap<String, Option<NamedModel>>,
    /// Top-level secrets and configs: one from a `file` is a bind mount.
    #[serde(default, deserialize_with = "nullable")]
    pub secrets: BTreeMap<String, Option<FileModel>>,
    #[serde(default, deserialize_with = "nullable")]
    pub configs: BTreeMap<String, Option<FileModel>>,
}

/// A top-level network or volume: what decides whether two Compose projects
/// share it, and what a node checks before it runs a lane's (ADR-0033).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct NamedModel {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub external: bool,
    pub driver: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub driver_opts: BTreeMap<String, serde_json::Value>,
    /// A network's addresses (`config: [{subnet, ...}]`).
    pub ipam: Option<serde_json::Value>,
    pub enable_ipv6: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub internal: bool,
}

/// A top-level secret or config.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileModel {
    pub file: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub external: bool,
    /// The variable of Compose's own environment it is read from.
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServiceModel {
    /// Values may be null (`environment: [NAME]` without a value).
    #[serde(default, deserialize_with = "nullable")]
    pub environment: BTreeMap<String, Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub ports: Vec<PortModel>,
    #[serde(default, deserialize_with = "nullable")]
    pub profiles: Vec<String>,
    pub container_name: Option<String>,
    pub image: Option<String>,
    pub build: Option<BuildModel>,
    // What reaches beyond the container: a node checks it (ADR-0033).
    #[serde(default, deserialize_with = "nullable")]
    pub privileged: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub cap_add: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub devices: Vec<serde_json::Value>,
    #[serde(default, deserialize_with = "nullable")]
    pub device_cgroup_rules: Vec<String>,
    pub pid: Option<String>,
    pub ipc: Option<String>,
    pub uts: Option<String>,
    pub cgroup: Option<String>,
    pub network_mode: Option<String>,
    pub userns_mode: Option<String>,
    pub cgroup_parent: Option<String>,
    pub runtime: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub security_opt: Vec<String>,
    /// A map or a list, as the file has it.
    pub sysctls: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "nullable")]
    pub volumes: Vec<MountModel>,
    #[serde(default, deserialize_with = "nullable")]
    pub volumes_from: Vec<String>,
    /// Paths (strings or `{path}`), when read with `--no-env-resolution`.
    #[serde(default, deserialize_with = "nullable")]
    pub env_file: Vec<serde_json::Value>,
    // What it needs of the project: `compose up <service>` starts or
    // creates these too.
    #[serde(default, deserialize_with = "nullable")]
    pub depends_on: BTreeMap<String, serde_json::Value>,
    #[serde(default, deserialize_with = "nullable")]
    pub links: Vec<String>,
    /// The networks it joins, by key (`config` writes a map).
    #[serde(default, deserialize_with = "nullable")]
    pub networks: BTreeMap<String, Option<serde_json::Value>>,
    /// `{source, target}` (or a name), as `config` writes them.
    #[serde(default, deserialize_with = "nullable")]
    pub secrets: Vec<serde_json::Value>,
    #[serde(default, deserialize_with = "nullable")]
    pub configs: Vec<serde_json::Value>,
}

/// A service's mount, in the long syntax `config` always writes.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MountModel {
    /// `bind`, `volume`, `tmpfs`, `npipe`, `image`, `cluster`.
    #[serde(rename = "type", default, deserialize_with = "nullable")]
    pub kind: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

/// A service's `build`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BuildModel {
    pub context: Option<String>,
    pub dockerfile: Option<String>,
    pub network: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub additional_contexts: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "nullable")]
    pub privileged: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub entitlements: Vec<String>,
    pub ssh: Option<serde_json::Value>,
    /// More names the built image is tagged with.
    #[serde(default, deserialize_with = "nullable")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub cache_from: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub cache_to: Vec<String>,
    /// Top-level secrets the build mounts (`{source, target}` or a name).
    #[serde(default, deserialize_with = "nullable")]
    pub secrets: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortModel {
    pub target: u16,
    /// Absent when Docker assigns the port at start.
    pub published: Option<String>,
    pub host_ip: Option<String>,
    pub protocol: Option<String>,
}

/// One line of `docker compose ps --format json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Container {
    pub service: String,
    pub name: String,
    /// `running`, `exited`, `restarting`, …
    pub state: String,
    /// `healthy`, `unhealthy`, `starting`, or empty without a health check.
    #[serde(default, deserialize_with = "nullable")]
    pub health: String,
    #[serde(default, deserialize_with = "nullable")]
    pub exit_code: i64,
    #[serde(default, deserialize_with = "nullable")]
    pub publishers: Vec<Publisher>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Publisher {
    #[serde(rename = "URL", default, deserialize_with = "nullable")]
    pub url: String,
    pub target_port: u16,
    #[serde(default, deserialize_with = "nullable")]
    pub published_port: u16,
}

// What a published port does when this machine connects to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// It answered, or it waits for the client to speak first (a database).
    Open,
    /// Nothing accepts the connection.
    Refused,
    /// The connection is accepted and closed at once, before a byte: Docker
    /// Desktop's port forwarding lost the container (seen after Docker Desktop
    /// restarted). The container itself can be healthy.
    ClosedAtOnce,
}

/// Connect to `addr` (`127.0.0.1:5432`) and listen for a moment: a server
/// either says something, or waits for the client; a dead port forward
/// closes the connection without a word.
pub fn probe(addr: &str) -> Reach {
    use std::io::Read;
    let Ok(sa) = addr.parse::<std::net::SocketAddr>() else {
        return Reach::Open; // not an address we can judge
    };
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&sa, Duration::from_secs(1)) else {
        return Reach::Refused;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(400)));
    let mut b = [0u8; 1];
    match s.read(&mut b) {
        Ok(0) => Reach::ClosedAtOnce,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => Reach::ClosedAtOnce,
        _ => Reach::Open,
    }
}

impl Container {
    /// Ready = running and, if it has a health check, healthy.
    pub fn is_ready(&self) -> bool {
        self.state == "running" && (self.health.is_empty() || self.health == "healthy")
    }

    /// Host address (`127.0.0.1:<port>`) at which `target` is published, if
    /// it is: loopback for a wildcard or loopback binding (IPv4 first), else
    /// the address the port is bound to.
    pub fn published(&self, target: u16) -> Option<String> {
        let mine = || {
            self.publishers
                .iter()
                .filter(move |p| p.target_port == target && p.published_port != 0)
        };
        if let Some(p) = mine().find(|p| matches!(p.url.as_str(), "127.0.0.1" | "0.0.0.0" | "")) {
            return Some(format!("127.0.0.1:{}", p.published_port));
        }
        let p = mine().next()?;
        Some(match p.url.as_str() {
            "::" | "::1" => format!("[::1]:{}", p.published_port),
            ip if ip.contains(':') => format!("[{ip}]:{}", p.published_port),
            ip => format!("{ip}:{}", p.published_port),
        })
    }
}

pub fn parse_config(json: &str) -> std::result::Result<Model, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

/// `ps --format json` prints one object per line (older versions: one array).
pub fn parse_ps(text: &str) -> std::result::Result<Vec<Container>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    if t.starts_with('[') {
        return serde_json::from_str(t).map_err(|e| e.to_string());
    }
    t.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| e.to_string()))
        .collect()
}

// ------------------------------------------------------------------- driver

/// How long a Docker call may take before devstack gives up on it and ends
/// it (a Docker Desktop that restarts holds every call). `up` and `create`
/// pull and build images: they get long budgets.
pub mod budget {
    use std::time::Duration;
    /// `config`, `ps`, `logs`, `network ...`, `stats`.
    pub const QUICK: Duration = Duration::from_secs(60);
    /// `stop`, `restart`, `down`: containers take their stop timeout.
    pub const STOP: Duration = Duration::from_secs(180);
    /// `system df -v`: Docker walks every volume.
    pub const SLOW: Duration = Duration::from_secs(300);
    /// `create`, and `up` on top of its health wait: pulls and builds.
    pub const PULL: Duration = Duration::from_secs(30 * 60);
}

/// Hide the console window a child would open on Windows (the daemon and
/// the hub have none).
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Run `cmd` to its end and collect its output, at most `budget`; `None`
/// when it took longer. It runs as a tree of its own
/// ([`devstack_proc::Call`]): over budget, the program and everything it
/// started end (`docker compose` runs Compose's plugin as a child, which
/// would otherwise go on and hold the pipes). Its streams are read while it
/// runs, so a chatty child cannot block on a full pipe. On Windows it runs
/// without a console window.
pub fn output_within(cmd: Command, budget: Duration) -> std::io::Result<Option<Output>> {
    output_until(cmd, budget, &|| false)
}

/// [`output_within`], ended early as well (`None`) once `stop` says so: a
/// lane's containers started on a node while the lane stops (#164).
pub fn output_until(
    mut cmd: Command,
    budget: Duration,
    stop: &dyn Fn() -> bool,
) -> std::io::Result<Option<Output>> {
    use std::io::Read;
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut call = devstack_proc::Call::spawn(&mut cmd)?;
    fn reader(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    }
    let out = reader(call.child().stdout.take());
    let err = reader(call.child().stderr.take());
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Some(s) = call.child().try_wait()? {
            break s;
        }
        if t0.elapsed() >= budget || stop() {
            call.kill();
            // With the tree gone the pipes close and the readers end. A
            // process that left the tree (a daemon it started) could still
            // hold them: the readers get a moment, then are left alone.
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while !(out.is_finished() && err.is_finished()) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if out.is_finished() && err.is_finished() {
                let _ = (out.join(), err.join());
            }
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // What the program left running (Windows: its job) ends with the call.
    drop(call);
    Ok(Some(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    }))
}

/// The variables a Docker CLI needs to reach its engine and its own
/// configuration (registry credentials, plugins, contexts), and those a
/// process needs on Windows; [`clean_env`] keeps these only.
const DOCKER_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "TMPDIR",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "DOCKER_API_VERSION",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "TEMP",
    "TMP",
    "PATHEXT",
    "COMSPEC",
];

/// Run `cmd` with only what the Docker CLI needs of this process's
/// environment ([`DOCKER_ENV`]): Compose resolves a service's
/// `environment: [NAME]`, a secret's `environment` and `${VAR}` from it, and
/// a node agent's environment is not a lane's to read (ADR-0033).
pub fn clean_env(cmd: &mut Command) {
    cmd.env_clear();
    for (k, v) in std::env::vars_os() {
        let name = k.to_string_lossy();
        let keep = DOCKER_ENV.iter().any(|d| {
            if cfg!(windows) {
                d.eq_ignore_ascii_case(&name)
            } else {
                *d == name
            }
        });
        if keep {
            cmd.env(k, v);
        }
    }
}

/// Runs the Docker CLI. `program` is `docker` unless a test substitutes it.
#[derive(Debug, Clone)]
pub struct Docker {
    pub program: String,
    /// Run it with [`clean_env`] (a node agent's calls), not with this
    /// process's whole environment.
    pub clean_env: bool,
}

impl Default for Docker {
    fn default() -> Self {
        Self {
            program: "docker".into(),
            clean_env: false,
        }
    }
}

impl Docker {
    /// The Docker CLI with only what it needs of the environment
    /// ([`clean_env`]).
    pub fn clean() -> Self {
        Self {
            clean_env: true,
            ..Self::default()
        }
    }

    fn command(&self, dir: &Path, args: &[OsString]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(args).current_dir(dir);
        if self.clean_env {
            clean_env(&mut cmd);
        }
        no_window(&mut cmd);
        cmd
    }

    fn run(&self, dir: &Path, args: &[OsString], budget: Duration) -> Result<Output> {
        self.run_until(dir, args, budget, &|| false)
    }

    fn run_until(
        &self,
        dir: &Path,
        args: &[OsString],
        budget: Duration,
        stop: &dyn Fn() -> bool,
    ) -> Result<Output> {
        let out = output_until(self.command(dir, args), budget, stop)
            .map_err(|source| ComposeError::Spawn {
                program: self.program.clone(),
                source,
            })?
            .ok_or_else(|| {
                if stop() {
                    ComposeError::Stopped {
                        command: describe(&self.program, args),
                    }
                } else {
                    ComposeError::Timeout {
                        command: describe(&self.program, args),
                        secs: budget.as_secs(),
                    }
                }
            })?;
        if !out.status.success() {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let tail: Vec<&str> = text.lines().rev().take(40).collect();
            return Err(ComposeError::Failed {
                command: describe(&self.program, args),
                status: out.status.to_string(),
                output: tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            });
        }
        Ok(out)
    }

    /// The fully interpolated model, with every profile enabled so that parked
    /// services (ADR-0004) can still serve as environment sources (ADR-0014).
    pub fn config(&self, g: &Group) -> Result<Model> {
        self.config_with(g, &[])
    }

    /// [`Docker::config`] with more flags for `config` (`--no-env-resolution`:
    /// services' `env_file` stay paths, unread).
    pub fn config_with(&self, g: &Group, flags: &[&str]) -> Result<Model> {
        let mut a = g.base_args();
        a.extend([
            "--profile".into(),
            "*".into(),
            "config".into(),
            "--format".into(),
            "json".into(),
        ]);
        a.extend(flags.iter().map(OsString::from));
        let out = self.run(&g.dir, &a, budget::QUICK)?;
        parse_config(&String::from_utf8_lossy(&out.stdout)).map_err(|message| ComposeError::Parse {
            command: describe(&self.program, &a),
            message,
        })
    }

    /// `config -q` for `devstack check` (ADR-0015: every group validates on its own).
    pub fn validate(&self, g: &Group) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["config".into(), "-q".into()]);
        self.run(&g.dir, &a, budget::QUICK).map(|_| ())
    }

    /// Create the group's own networks unless they exist.
    pub fn ensure_networks(&self, g: &Group) -> Result<()> {
        for n in &g.networks {
            self.ensure_network(&g.dir, n, &[])?;
        }
        Ok(())
    }

    /// Create the network `name` with `flags` (`--internal`) unless it
    /// exists. Created first, "exists already" taken as success: two groups
    /// (or two devstacks) that need one at once do not race.
    pub fn ensure_network(&self, dir: &Path, name: &str, flags: &[&str]) -> Result<()> {
        let mut a: Vec<OsString> = vec!["network".into(), "create".into()];
        a.extend(flags.iter().map(OsString::from));
        a.push(name.into());
        match self.run(dir, &a, budget::QUICK) {
            Ok(_) => Ok(()),
            Err(ComposeError::Failed { output, .. }) if output.contains("already exists") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Create the group's selected services and their networks without
    /// starting anything (rules for the networks can go in first).
    pub fn create(&self, g: &Group) -> Result<()> {
        self.ensure_networks(g)?;
        let mut a = g.base_args();
        a.extend(["create".into(), "--no-recreate".into()]);
        a.extend(g.services.iter().map(OsString::from));
        self.run(&g.dir, &a, budget::PULL).map(|_| ())
    }

    /// Start exactly the group's selected services without waiting for
    /// their health (Compose still waits for the `depends_on` conditions
    /// among them). [`Docker::up`] afterwards waits.
    pub fn up_detached(&self, g: &Group) -> Result<()> {
        self.ensure_networks(g)?;
        let mut a = g.base_args();
        a.extend(["up".into(), "-d".into(), "--no-deps".into()]);
        a.extend(g.services.iter().map(OsString::from));
        self.run(&g.dir, &a, budget::PULL).map(|_| ())
    }

    /// Start exactly the group's selected services and wait until they are
    /// running and healthy (`--wait`), at most `timeout` for that, on top of
    /// what pulling and building their images takes.
    pub fn up(&self, g: &Group, timeout: Duration) -> Result<()> {
        self.ensure_networks(g)?;
        let mut a = g.base_args();
        a.extend([
            "up".into(),
            "-d".into(),
            "--wait".into(),
            "--wait-timeout".into(),
            timeout.as_secs().max(1).to_string().into(),
            "--no-deps".into(),
        ]);
        a.extend(g.services.iter().map(OsString::from));
        self.run(&g.dir, &a, budget::PULL + timeout).map(|_| ())
    }

    /// Start the group's created services as they are, without building
    /// anything (`--no-build`: their images were built with
    /// [`Docker::create`]), ended early once `stop` says so. `wait`: and
    /// wait until they are running and healthy, at most that long. On a
    /// node, where the lane runs by then and could change its build
    /// contexts (#164, ADR-0033 point 11).
    pub fn start_created(
        &self,
        g: &Group,
        wait: Option<Duration>,
        stop: &dyn Fn() -> bool,
    ) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["up".into(), "-d".into()]);
        if let Some(t) = wait {
            a.extend([
                "--wait".into(),
                "--wait-timeout".into(),
                t.as_secs().max(1).to_string().into(),
            ]);
        }
        a.extend(["--no-deps".into(), "--no-build".into()]);
        a.extend(g.services.iter().map(OsString::from));
        let budget = budget::PULL + wait.unwrap_or_default();
        self.run_until(&g.dir, &a, budget, stop).map(|_| ())
    }

    pub fn ps(&self, g: &Group) -> Result<Vec<Container>> {
        let mut a = g.base_args();
        a.extend([
            "ps".into(),
            "--all".into(),
            "--format".into(),
            "json".into(),
        ]);
        let out = self.run(&g.dir, &a, budget::QUICK)?;
        parse_ps(&String::from_utf8_lossy(&out.stdout)).map_err(|message| ComposeError::Parse {
            command: describe(&self.program, &a),
            message,
        })
    }

    pub fn restart(&self, g: &Group, service: &str) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["restart".into(), service.into()]);
        self.run(&g.dir, &a, budget::STOP).map(|_| ())
    }

    /// `docker compose logs --follow` of one container: the last `tail`
    /// lines, then every new one, timestamped, on the child's stdout and
    /// stderr (the container's two streams). The caller reads and ends it.
    pub fn follow_logs(
        &self,
        g: &Group,
        service: &str,
        tail: usize,
    ) -> Result<std::process::Child> {
        let mut a = g.base_args();
        a.extend([
            "logs".into(),
            "--follow".into(),
            "--no-color".into(),
            "--no-log-prefix".into(),
            "--timestamps".into(),
            "--tail".into(),
            tail.to_string().into(),
            service.into(),
        ]);
        self.command(&g.dir, &a)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|source| ComposeError::Spawn {
                program: self.program.clone(),
                source,
            })
    }

    /// Stop one container; it stays (and starts again with `up`).
    pub fn stop(&self, g: &Group, service: &str) -> Result<()> {
        let mut a = g.base_args();
        a.extend([
            "stop".into(),
            "--timeout".into(),
            "10".into(),
            service.into(),
        ]);
        self.run(&g.dir, &a, budget::STOP).map(|_| ())
    }

    /// Stop a container by its name (`docker stop`), whether or not the
    /// loaded Compose model knows its service.
    pub fn stop_container(&self, dir: &Path, name: &str) -> Result<()> {
        let a: Vec<OsString> = vec![
            "stop".into(),
            "--timeout".into(),
            "10".into(),
            "--".into(),
            name.into(),
        ];
        self.run(dir, &a, budget::STOP).map(|_| ())
    }

    /// The last `tail` lines of a container's output, each starting with its
    /// RFC 3339 timestamp; `since` as Docker takes it (`30m`, a timestamp).
    /// Docker hands stdout and stderr back separately; the timestamps put
    /// them into one order again.
    pub fn logs(
        &self,
        g: &Group,
        service: &str,
        tail: usize,
        since: Option<&str>,
    ) -> Result<String> {
        let mut a = g.base_args();
        a.extend([
            "logs".into(),
            "--no-color".into(),
            "--no-log-prefix".into(),
            "--timestamps".into(),
            "--tail".into(),
            tail.to_string().into(),
        ]);
        if let Some(s) = since {
            a.extend(["--since".into(), s.into()]);
        }
        a.push(service.into());
        let out = self.run(&g.dir, &a, budget::QUICK)?;
        Ok(merge_by_time(
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
            tail,
        ))
    }

    /// Stop and remove the group's containers (volumes are kept).
    pub fn down(&self, g: &Group) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["down".into(), "--timeout".into(), "10".into()]);
        self.run(&g.dir, &a, budget::STOP).map(|_| ())
    }

    /// Remove the group's containers, its volumes and the networks devstack
    /// created for it: a lane whose worktree is gone (ADR-0030).
    pub fn remove(&self, g: &Group) -> Result<()> {
        let mut a = g.base_args();
        a.extend([
            "down".into(),
            "--volumes".into(),
            "--remove-orphans".into(),
            "--timeout".into(),
            "10".into(),
        ]);
        self.run(&g.dir, &a, budget::STOP)?;
        for n in &g.networks {
            let _ = self.run(
                &g.dir,
                &["network".into(), "rm".into(), n.into()],
                budget::QUICK,
            );
        }
        Ok(())
    }
}

/// The Compose overlay that makes a lane's copy of a group its own (ADR-0030):
/// every published host port moved by `offset`, no fixed `container_name`,
/// and every network or volume that has a fixed name renamed `<name>-<lane>`,
/// so nothing is shared with the main tree or another lane. An external
/// network is renamed only when it is one of the group's own `networks`
/// (`own`, the stack file's names), which devstack creates per lane; any
/// other external network is someone else's and stays as it is, like
/// external volumes, which hold data someone put there on purpose. `model`
/// is the group's configuration under the lane's `project`, whose own names
/// (`<project>_<key>`) are its own already. `keep` are services the lane does
/// not run (`shared`); they are left alone. YAML, with the merge tags of
/// Compose 2.24 (`!override`, `!reset`).
pub fn lane_overlay(
    model: &Model,
    project: &str,
    lane: &str,
    offset: u16,
    keep: &[String],
    own: &[String],
) -> std::result::Result<String, String> {
    Ok(overlay(model, project, lane, Some(offset), keep, own)?.render(lane))
}

/// What a node adds to a lane's overlay (ADR-0033).
#[derive(Debug, Clone, Default)]
pub struct NodeOptions {
    /// The group's own networks, as the stack file names them (ADR-0030).
    pub own_networks: Vec<String>,
    /// Every network of the lane `internal`: its containers reach each other
    /// and nothing else (a node's `none`).
    pub isolated: bool,
    /// No IPv6 on the lane's networks: the node's egress rules are IPv4's.
    pub no_ipv6: bool,
    /// Images are built without a network (`build.network: none`).
    pub build_offline: bool,
    /// Limits of every container: memory (bytes), CPU (cores), processes.
    pub memory: Option<u64>,
    pub cpus: Option<f64>,
    pub pids: Option<u32>,
    /// The image name a service with `build` is built as: one the node
    /// chose, so another lane cannot name it.
    pub build_images: BTreeMap<String, String>,
}

/// The overlay for a lane on a node (ADR-0025): as [`lane_overlay`], but
/// nothing is published on the node's host at all (the lane container
/// reaches every service by its name on the lane's network, and two lanes on
/// one node would otherwise want the same host ports), with what the node's
/// `options` add.
pub fn node_overlay(
    model: &Model,
    project: &str,
    lane: &str,
    options: &NodeOptions,
) -> std::result::Result<String, String> {
    let mut o = overlay(model, project, lane, None, &[], &options.own_networks)?;
    for (name, svc) in &model.services {
        let body = o.services.entry(name.clone()).or_default();
        if options.memory.is_some() || options.cpus.is_some() || options.pids.is_some() {
            // One place for the limits: Compose refuses `mem_limit` beside a
            // different `deploy.resources.limits.memory`.
            body.push("    mem_limit: !reset null".into());
            body.push("    memswap_limit: !reset null".into());
            body.push("    cpus: !reset null".into());
            body.push("    pids_limit: !reset null".into());
            body.push("    deploy:\n      resources:\n        limits:".into());
            if let Some(m) = options.memory {
                // A string with its unit: Compose takes no bare number here.
                body.push(format!("          memory: \"{m}b\""));
            }
            if let Some(c) = options.cpus {
                body.push(format!("          cpus: {}", quote(&format!("{c:.2}"))));
            }
            if let Some(p) = options.pids {
                body.push(format!("          pids: {p}"));
            }
        }
        if svc.build.is_some() {
            if options.build_offline {
                body.push("    build:\n      network: none".into());
            }
            if let Some(image) = options.build_images.get(name) {
                body.push(format!("    image: {}", quote(image)));
            }
        }
    }
    if options.isolated || options.no_ipv6 {
        // The `default` network always exists for services without networks.
        let mut keys: Vec<&str> = model
            .networks
            .iter()
            .filter(|(_, m)| !m.as_ref().is_some_and(|m| m.external))
            .map(|(k, _)| k.as_str())
            .collect();
        if !model.networks.contains_key("default") {
            keys.push("default");
        }
        for k in keys {
            let body = o.networks.entry(k.to_string()).or_default();
            if options.isolated {
                body.push("    internal: true".into());
            }
            if options.no_ipv6 {
                body.push("    enable_ipv6: false".into());
            }
        }
    }
    Ok(o.render(lane))
}

/// A YAML string.
fn quote(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// An overlay as it is put together: the lines under each service, network
/// and volume, by key.
#[derive(Default)]
struct Overlay {
    services: BTreeMap<String, Vec<String>>,
    networks: BTreeMap<String, Vec<String>>,
    volumes: BTreeMap<String, Vec<String>>,
}

impl Overlay {
    fn render(&self, lane: &str) -> String {
        let mut out = format!(
            "# Written by devstack for lane `{lane}` (ADR-0030): its own ports, names,\n# networks and volumes. Do not edit; it is written again at every start.\n"
        );
        for (kind, items) in [
            ("services", &self.services),
            ("networks", &self.networks),
            ("volumes", &self.volumes),
        ] {
            let items: Vec<_> = items.iter().filter(|(_, b)| !b.is_empty()).collect();
            if items.is_empty() {
                continue;
            }
            out.push_str(kind);
            out.push_str(":\n");
            for (key, body) in items {
                out.push_str(&format!("  {}:\n", quote(key)));
                for line in body {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        out
    }
}

/// `offset`: move published ports by it; `None`: publish none.
fn overlay(
    model: &Model,
    project: &str,
    lane: &str,
    offset: Option<u16>,
    keep: &[String],
    own: &[String],
) -> std::result::Result<Overlay, String> {
    let by = offset.unwrap_or(0);
    let shift = |p: &str| -> std::result::Result<String, String> {
        let one = |n: &str| -> std::result::Result<String, String> {
            let n: u16 = n
                .trim()
                .parse()
                .map_err(|_| format!("port `{p}` is not a number"))?;
            n.checked_add(by)
                .map(|m| m.to_string())
                .ok_or_else(|| format!("port {n} + {by} is beyond 65535"))
        };
        match p.split_once('-') {
            Some((a, b)) => Ok(format!("{}-{}", one(a)?, one(b)?)),
            None => one(p),
        }
    };
    let mut o = Overlay::default();
    for (name, svc) in model.services.iter().filter(|(n, _)| !keep.contains(n)) {
        let mut body = Vec::new();
        if !svc.ports.is_empty() && offset.is_none() {
            body.push("    ports: !reset []".to_string());
        } else if !svc.ports.is_empty() {
            body.push("    ports: !override".to_string());
            for port in &svc.ports {
                let mut spec = String::new();
                if let Some(published) = &port.published {
                    if let Some(ip) = port.host_ip.as_deref().filter(|ip| !ip.is_empty()) {
                        spec.push_str(ip);
                        spec.push(':');
                    }
                    spec.push_str(&shift(published)?);
                    spec.push(':');
                }
                spec.push_str(&port.target.to_string());
                if let Some(proto) = port.protocol.as_deref().filter(|p| *p != "tcp") {
                    spec.push('/');
                    spec.push_str(proto);
                }
                body.push(format!("      - {}", quote(&spec)));
            }
        }
        if svc.container_name.is_some() {
            body.push("    container_name: !reset null".to_string());
        }
        if !body.is_empty() {
            o.services.insert(name.clone(), body);
        }
    }
    let renamed = |items: &BTreeMap<String, Option<NamedModel>>,
                   external: &dyn Fn(&str) -> bool| {
        let mut out = BTreeMap::new();
        for (key, m) in items {
            let Some(m) = m else { continue };
            let name = m.name.as_deref().unwrap_or(key);
            if m.external && !external(name) {
                continue;
            }
            if !m.external && (m.name.is_none() || name == format!("{project}_{key}")) {
                continue; // project-scoped already
            }
            out.insert(
                key.clone(),
                vec![format!("    name: {}", quote(&format!("{name}-{lane}")))],
            );
        }
        out
    };
    o.networks = renamed(&model.networks, &|n| own.iter().any(|o| o == n));
    o.volumes = renamed(&model.volumes, &|_| false);
    Ok(o)
}

// Two timestamped streams as one, in time order, the last `tail` lines.
/// RFC 3339 timestamps of one container sort as text; a line without one
/// (a continuation) stays after the line before it.
pub fn merge_by_time(stdout: &str, stderr: &str, tail: usize) -> String {
    let mut lines: Vec<(&str, usize, &str)> = Vec::new();
    for (i, text) in [stdout, stderr].into_iter().enumerate() {
        let mut key = "";
        for l in text.lines() {
            if let Some((ts, _)) = l
                .split_once(' ')
                .filter(|(t, _)| t.len() >= 20 && t.as_bytes()[4] == b'-')
            {
                key = ts;
            }
            lines.push((key, i, l));
        }
    }
    lines.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
    let skip = lines.len().saturating_sub(tail);
    lines
        .into_iter()
        .skip(skip)
        .map(|(_, _, l)| l)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Live load of one container (`docker stats`, plan §12).
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerStats {
    pub name: String,
    /// Percent of one core.
    pub cpu: f64,
    /// Bytes.
    pub memory: u64,
}

/// One named volume with its size (`docker system df -v`, plan §12).
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeSize {
    pub name: String,
    pub project: Option<String>,
    pub size: u64,
}

impl Docker {
    /// One `docker stats --no-stream` sample of the named containers. Takes
    /// about two seconds: Docker measures over an interval.
    pub fn stats(&self, names: &[String]) -> Result<Vec<ContainerStats>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut a: Vec<OsString> = vec![
            "stats".into(),
            "--no-stream".into(),
            "--format".into(),
            "json".into(),
        ];
        a.extend(names.iter().map(OsString::from));
        let out = self.run(Path::new("."), &a, budget::QUICK)?;
        parse_stats(&String::from_utf8_lossy(&out.stdout)).map_err(|message| ComposeError::Parse {
            command: describe(&self.program, &a),
            message,
        })
    }

    /// Every volume with its size. Slow (Docker walks the volumes): sample rarely.
    pub fn volumes(&self) -> Result<Vec<VolumeSize>> {
        let a: Vec<OsString> = vec![
            "system".into(),
            "df".into(),
            "-v".into(),
            "--format".into(),
            "json".into(),
        ];
        let out = self.run(Path::new("."), &a, budget::SLOW)?;
        parse_volumes(&String::from_utf8_lossy(&out.stdout)).map_err(|message| {
            ComposeError::Parse {
                command: describe(&self.program, &a),
                message,
            }
        })
    }
}

pub fn parse_stats(text: &str) -> std::result::Result<Vec<ContainerStats>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Line {
        name: String,
        #[serde(rename = "CPUPerc")]
        cpu_perc: String,
        mem_usage: String,
    }
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let line: Line = serde_json::from_str(l).map_err(|e| e.to_string())?;
            let used = line.mem_usage.split('/').next().unwrap_or("").trim();
            Ok(ContainerStats {
                name: line.name,
                cpu: line
                    .cpu_perc
                    .trim()
                    .trim_end_matches('%')
                    .parse()
                    .unwrap_or(0.0),
                memory: parse_size(used).unwrap_or(0),
            })
        })
        .collect()
}

pub fn parse_volumes(json: &str) -> std::result::Result<Vec<VolumeSize>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Df {
        #[serde(default)]
        volumes: Vec<Volume>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Volume {
        name: String,
        #[serde(default)]
        labels: String,
        #[serde(default)]
        size: String,
    }
    let df: Df = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(df
        .volumes
        .into_iter()
        .map(|v| VolumeSize {
            project: v
                .labels
                .split(',')
                .find_map(|kv| kv.strip_prefix("com.docker.compose.project="))
                .map(str::to_string),
            size: parse_size(&v.size).unwrap_or(0),
            name: v.name,
        })
        .collect())
}

/// Docker's human sizes: `12B`, `37.56kB`, `1.2MB` (SI), `35.27MiB`, `31.35GiB` (binary).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult: f64 = match unit.trim() {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((n * mult).round() as u64)
}

fn describe(program: &str, args: &[OsString]) -> String {
    let mut s = program.to_string();
    for a in args {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_tells_a_lost_forward_from_a_silent_server() {
        use std::net::TcpListener;
        // Accepts and closes at once, like Docker Desktop with a lost forward.
        let closing = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = closing.local_addr().unwrap().to_string();
        let t = std::thread::spawn(move || {
            let (s, _) = closing.accept().unwrap();
            drop(s);
        });
        assert_eq!(probe(&a), Reach::ClosedAtOnce);
        t.join().unwrap();
        // Accepts and waits for the client, like a database.
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = silent.local_addr().unwrap().to_string();
        let t = std::thread::spawn(move || {
            let (s, _) = silent.accept().unwrap();
            std::thread::sleep(Duration::from_millis(800));
            drop(s);
        });
        assert_eq!(probe(&b), Reach::Open);
        t.join().unwrap();
        // Nobody listens.
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let c = free.local_addr().unwrap().to_string();
        drop(free);
        assert_eq!(probe(&c), Reach::Refused);
    }

    #[test]
    fn logs_merge_stdout_and_stderr_by_time() {
        let out = "2026-10-15T10:00:01.000000000Z ready\n2026-10-15T10:00:03.000000000Z query ok";
        let err = "2026-10-15T10:00:02.000000000Z ERROR: relation missing\n\tat line 4";
        assert_eq!(
            merge_by_time(out, err, 10),
            "2026-10-15T10:00:01.000000000Z ready\n2026-10-15T10:00:02.000000000Z ERROR: relation missing\n\tat line 4\n2026-10-15T10:00:03.000000000Z query ok"
        );
        assert_eq!(
            merge_by_time(out, err, 1),
            "2026-10-15T10:00:03.000000000Z query ok"
        );
    }

    const CONFIG: &str = r#"{"name":"probe","services":{
      "api":{"profiles":["container-app"],"environment":{"DB_URL":"http://db:5432/x","EMPTY":null},
             "ports":[{"mode":"ingress","host_ip":"127.0.0.1","target":8080,"published":"18080","protocol":"tcp"}]},
      "db":{"ports":[{"mode":"ingress","target":8081,"protocol":"tcp"}]}}}"#;

    const PS: &str = r#"{"Service":"db","Name":"probe-db-1","State":"running","Health":"healthy","ExitCode":0,"Publishers":[{"URL":"::","TargetPort":8081,"PublishedPort":54804,"Protocol":"tcp"},{"URL":"0.0.0.0","TargetPort":8081,"PublishedPort":54804,"Protocol":"tcp"},{"URL":"127.0.0.1","TargetPort":5432,"PublishedPort":15432,"Protocol":"tcp"}],"Status":"Up"}
{"Service":"queue","Name":"probe-queue-1","State":"running","Health":"starting","Publishers":[]}"#;

    #[test]
    fn stats_and_sizes() {
        let text = r#"{"BlockIO":"1.63GB / 0B","CPUPerc":"12.50%","Container":"cb21","ID":"cb21","MemPerc":"0.11%","MemUsage":"35.27MiB / 31.35GiB","Name":"probe-db-1","NetIO":"1MB / 1MB","PIDs":"11"}"#;
        let s = parse_stats(text).unwrap();
        assert_eq!(s[0].name, "probe-db-1");
        assert_eq!(s[0].cpu, 12.5);
        assert_eq!(s[0].memory, (35.27 * 1024.0 * 1024.0f64).round() as u64);
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("37.56kB"), Some(37_560));
        assert_eq!(parse_size("1.5GiB"), Some(1_610_612_736));
        assert_eq!(parse_size("N/A"), None);
        let df = r#"{"Images":[],"Volumes":[{"Labels":"com.docker.compose.project=probe,com.docker.compose.volume=data","Name":"probe_data","Size":"122.4MB"},{"Labels":"","Name":"loose","Size":"N/A"}]}"#;
        let v = parse_volumes(df).unwrap();
        assert_eq!(v[0].project.as_deref(), Some("probe"));
        assert_eq!(v[0].size, 122_400_000);
        assert_eq!((v[1].project.as_deref(), v[1].size), (None, 0));
    }

    #[test]
    fn config_model() {
        let m = parse_config(CONFIG).unwrap();
        let api = &m.services["api"];
        assert_eq!(
            api.environment["DB_URL"].as_deref(),
            Some("http://db:5432/x")
        );
        assert_eq!(api.environment["EMPTY"], None);
        assert_eq!(api.ports[0].published.as_deref(), Some("18080"));
        assert_eq!(m.services["db"].ports[0].published, None);
        assert_eq!(api.profiles, ["container-app"]);
    }

    #[test]
    fn ps_lines_and_publishers() {
        let c = parse_ps(PS).unwrap();
        assert_eq!(c.len(), 2);
        assert!(c[0].is_ready() && !c[1].is_ready());
        assert_eq!(c[0].published(8081).as_deref(), Some("127.0.0.1:54804"));
        assert_eq!(c[0].published(5432).as_deref(), Some("127.0.0.1:15432"));
        assert_eq!(c[0].published(9999), None);
        assert!(parse_ps("").unwrap().is_empty());
        assert_eq!(parse_ps("[]").unwrap().len(), 0);
    }

    #[test]
    fn a_clean_environment_keeps_what_docker_needs_only() {
        let mut cmd = Command::new("docker");
        clean_env(&mut cmd);
        let kept: Vec<String> = cmd
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().to_uppercase())
            .collect();
        assert!(
            kept.iter().all(|k| DOCKER_ENV.contains(&k.as_str())),
            "{kept:?}"
        );
        if std::env::var_os("PATH").is_some() {
            assert!(kept.iter().any(|k| k == "PATH"), "{kept:?}");
        }
        assert!(Docker::clean().clean_env && !Docker::default().clean_env);
    }

    #[test]
    fn base_args_name_project_and_files_and_skip_missing_env_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.defaults"), "").unwrap();
        let g = Group {
            name: "app".into(),
            project: "p-dev".into(),
            files: vec![dir.path().join("a.yml"), dir.path().join("b.yml")],
            env_files: vec![dir.path().join(".env.defaults"), dir.path().join(".env")],
            networks: vec![],
            services: vec!["db".into()],
            dir: dir.path().into(),
        };
        let a: Vec<String> = g
            .base_args()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&a[..3], ["compose", "-p", "p-dev"]);
        assert_eq!(a.iter().filter(|x| *x == "-f").count(), 2);
        assert_eq!(
            a.iter().filter(|x| *x == "--env-file").count(),
            1,
            "missing .env must be skipped: {a:?}"
        );
    }

    #[test]
    fn a_lane_overlay_moves_ports_and_renames_what_projects_would_share() {
        let model = parse_config(
            r#"{
              "services": {
                "db": {"container_name": "shop-db",
                       "ports": [{"target": 5432, "published": "5432", "host_ip": "127.0.0.1", "protocol": "tcp"}]},
                "dns": {"ports": [{"target": 53, "published": "5353", "protocol": "udp"},
                                  {"target": 8080}]},
                "mail": {"ports": [{"target": 8025, "published": "8025"}]},
                "worker": {}
              },
              "networks": {"default": {"name": "shop-fix_default"}, "edge": {"name": "edge", "external": true}},
              "volumes": {"data": {"name": "shop-fix_data"}, "fixed": {"name": "shared-files"}, "keep": {"name": "kept", "external": true}}
            }"#,
        )
        .unwrap();
        let y = lane_overlay(
            &model,
            "shop-fix",
            "fix",
            100,
            &["mail".to_string()],
            &["edge".to_string()],
        )
        .unwrap();
        assert!(y.contains("\"db\":\n    ports: !override\n      - \"127.0.0.1:5532:5432\"\n    container_name: !reset null"), "{y}");
        assert!(
            y.contains("- \"5453:53/udp\"") && y.contains("- \"8080\""),
            "{y}"
        );
        assert!(
            !y.contains("\"mail\""),
            "a shared service stays as it is: {y}"
        );
        assert!(!y.contains("\"worker\""), "{y}");
        assert!(y.contains("\"edge\":\n    name: \"edge-fix\""), "{y}");
        assert!(
            !y.contains("default") && !y.contains("shop-fix_data"),
            "names of the lane's project are its own already: {y}"
        );
        assert!(
            y.contains("\"fixed\":\n    name: \"shared-files-fix\""),
            "{y}"
        );
        assert!(!y.contains("kept"), "external volumes stay: {y}");
        let over = lane_overlay(&model, "shop-fix", "fix", 65000, &[], &[]);
        assert!(over.is_err());
    }

    #[test]
    fn a_node_overlay_publishes_nothing() {
        let model = parse_config(
            r#"{"services":{"db":{"ports":[{"target":5432,"published":"5432"}],"container_name":"shop-db"},
                "cache":{}},"networks":{"default":{"name":"shop_default"}}}"#,
        )
        .unwrap();
        let y = node_overlay(&model, "shop-ab12", "ab12", &NodeOptions::default()).unwrap();
        assert!(y.contains("ports: !reset []"), "{y}");
        assert!(y.contains("container_name: !reset null"), "{y}");
        assert!(!y.contains("5432"), "{y}");
        let isolated = NodeOptions {
            isolated: true,
            ..NodeOptions::default()
        };
        let y = node_overlay(&model, "shop-ab12", "ab12", &isolated).unwrap();
        assert!(y.contains("internal: true"), "{y}");
    }

    #[test]
    fn an_external_network_is_renamed_only_when_the_group_owns_it() {
        let model = parse_config(
            r#"{"services":{"web":{}},
                "networks":{"edge":{"name":"edge","external":true},"proxy":{"name":"proxy","external":true}}}"#,
        )
        .unwrap();
        let y = lane_overlay(&model, "shop-fix", "fix", 0, &[], &["edge".into()]).unwrap();
        assert!(
            y.contains(
                "\"edge\":
    name: \"edge-fix\""
            ),
            "{y}"
        );
        assert!(
            !y.contains("proxy"),
            "someone else's network stays as it is: {y}"
        );
    }

    #[test]
    fn an_isolated_node_overlay_marks_networks_not_services_named_alike() {
        // A service named like a network: the old text search put
        // `internal: true` under the service.
        let model = parse_config(
            r#"{"services":{"backend":{"build":{"context":"/src/api"}},"db":{}},
                "networks":{"backend":{"name":"shop_backend"},"default":{"name":"shop-ab12_default"}}}"#,
        )
        .unwrap();
        let o = NodeOptions {
            isolated: true,
            no_ipv6: true,
            build_offline: true,
            memory: Some(2 << 30),
            cpus: Some(1.5),
            pids: Some(512),
            build_images: [("backend".to_string(), "devstack-build-x".to_string())].into(),
            ..NodeOptions::default()
        };
        let y = node_overlay(&model, "shop-ab12", "ab12", &o).unwrap();
        let services = &y[y.find("services:").unwrap()..y.find("networks:").unwrap()];
        let networks = &y[y.find("networks:").unwrap()..];
        assert!(!services.contains("internal"), "{y}");
        assert!(
            networks.contains(
                "  \"backend\":
    name: \"shop_backend-ab12\"
    internal: true
    enable_ipv6: false"
            ),
            "{y}"
        );
        assert!(
            networks.contains(
                "  \"default\":
    internal: true"
            ),
            "{y}"
        );
        assert!(
            services.contains("memory: \"2147483648b\"") && services.contains("cpus: \"1.50\""),
            "{y}"
        );
        assert!(services.contains("pids: 512"), "{y}");
        assert!(
            services.contains(
                "build:
      network: none"
            ),
            "{y}"
        );
        assert!(services.contains("image: \"devstack-build-x\""), "{y}");
    }

    #[test]
    fn compose_nulls_read_as_empty() {
        let m = parse_config(
            r#"{"services":{"db":{"environment":null,"ports":null,"profiles":null,"volumes":null,"cap_add":null}},"networks":null,"volumes":null}"#,
        )
        .unwrap();
        assert!(m.services["db"].ports.is_empty());
        let c = parse_ps(
            r#"{"Service":"db","Name":"x","State":"running","Health":"","Publishers":null}"#,
        )
        .unwrap();
        assert!(c[0].publishers.is_empty());
    }

    #[test]
    fn a_port_bound_to_one_address_is_reached_there() {
        let c = parse_ps(
            r#"{"Service":"db","Name":"x","State":"running","Publishers":[{"URL":"10.0.0.7","TargetPort":5432,"PublishedPort":15432},{"URL":"::","TargetPort":80,"PublishedPort":8080},{"URL":"fd00::1","TargetPort":81,"PublishedPort":8081}]}"#,
        )
        .unwrap();
        assert_eq!(c[0].published(5432).as_deref(), Some("10.0.0.7:15432"));
        assert_eq!(c[0].published(80).as_deref(), Some("[::1]:8080"));
        assert_eq!(c[0].published(81).as_deref(), Some("[fd00::1]:8081"));
    }

    #[test]
    fn a_call_that_takes_too_long_is_ended() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 20"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("20");
            c
        };
        no_window(&mut cmd);
        let t0 = std::time::Instant::now();
        assert!(
            output_within(cmd, Duration::from_millis(300))
                .unwrap()
                .is_none()
        );
        assert!(t0.elapsed() < Duration::from_secs(10));
        let mut echo = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo hi"]);
            c
        } else {
            let mut c = Command::new("echo");
            c.arg("hi");
            c
        };
        no_window(&mut echo);
        let out = output_within(echo, Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }
}
