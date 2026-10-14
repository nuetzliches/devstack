//! The infra plane (ADR-0004): devstack drives the project's own Compose files
//! and never manages containers itself. Every call names the project and the
//! files explicitly; `up` starts exactly the selected services with
//! `--no-deps`, ordering is devstack's job.
//!
//! Formats verified against Docker Compose v5.6: `config --format json` gives
//! `published` as a string and omits it for dynamically assigned ports;
//! `ps --format json` prints one JSON object per line, with `Publishers`.

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
    #[error("cannot read the output of `{command}`: {message}")]
    Parse { command: String, message: String },
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

/// The parts of `docker compose config --format json` devstack reads.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Model {
    #[serde(default)]
    pub services: BTreeMap<String, ServiceModel>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServiceModel {
    /// Values may be null (`environment: [NAME]` without a value).
    #[serde(default)]
    pub environment: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub ports: Vec<PortModel>,
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortModel {
    pub target: u16,
    /// Absent when Docker assigns the port at start.
    pub published: Option<String>,
    pub host_ip: Option<String>,
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
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub exit_code: i64,
    #[serde(default)]
    pub publishers: Vec<Publisher>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Publisher {
    #[serde(rename = "URL")]
    pub url: String,
    pub target_port: u16,
    pub published_port: u16,
}

impl Container {
    /// Ready = running and, if it has a health check, healthy.
    pub fn is_ready(&self) -> bool {
        self.state == "running" && (self.health.is_empty() || self.health == "healthy")
    }

    /// Host address (`127.0.0.1:<port>`) at which `target` is published, if it is.
    pub fn published(&self, target: u16) -> Option<String> {
        self.publishers
            .iter()
            .filter(|p| p.target_port == target && p.published_port != 0)
            // IPv4 first: a loopback or wildcard binding is reachable on 127.0.0.1.
            .find(|p| matches!(p.url.as_str(), "127.0.0.1" | "0.0.0.0" | ""))
            .or_else(|| {
                self.publishers
                    .iter()
                    .find(|p| p.target_port == target && p.published_port != 0)
            })
            .map(|p| format!("127.0.0.1:{}", p.published_port))
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

/// Runs the Docker CLI. `program` is `docker` unless a test substitutes it.
#[derive(Debug, Clone)]
pub struct Docker {
    pub program: String,
}

impl Default for Docker {
    fn default() -> Self {
        Self {
            program: "docker".into(),
        }
    }
}

impl Docker {
    fn run(&self, dir: &Path, args: &[OsString]) -> Result<Output> {
        let out = Command::new(&self.program)
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|source| ComposeError::Spawn {
                program: self.program.clone(),
                source,
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
        let mut a = g.base_args();
        a.extend([
            "--profile".into(),
            "*".into(),
            "config".into(),
            "--format".into(),
            "json".into(),
        ]);
        let out = self.run(&g.dir, &a)?;
        parse_config(&String::from_utf8_lossy(&out.stdout)).map_err(|message| ComposeError::Parse {
            command: describe(&self.program, &a),
            message,
        })
    }

    /// `config -q` for `devstack check` (ADR-0015: every group validates on its own).
    pub fn validate(&self, g: &Group) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["config".into(), "-q".into()]);
        self.run(&g.dir, &a).map(|_| ())
    }

    pub fn ensure_networks(&self, g: &Group) -> Result<()> {
        for n in &g.networks {
            let exists = Command::new(&self.program)
                .args(["network", "inspect", n])
                .output()
                .map_err(|source| ComposeError::Spawn {
                    program: self.program.clone(),
                    source,
                })?
                .status
                .success();
            if !exists {
                self.run(&g.dir, &["network".into(), "create".into(), n.into()])?;
            }
        }
        Ok(())
    }

    /// Start exactly the group's selected services and wait until they are
    /// running and healthy (`--wait`), at most `timeout`.
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
        self.run(&g.dir, &a).map(|_| ())
    }

    pub fn ps(&self, g: &Group) -> Result<Vec<Container>> {
        let mut a = g.base_args();
        a.extend([
            "ps".into(),
            "--all".into(),
            "--format".into(),
            "json".into(),
        ]);
        let out = self.run(&g.dir, &a)?;
        parse_ps(&String::from_utf8_lossy(&out.stdout)).map_err(|message| ComposeError::Parse {
            command: describe(&self.program, &a),
            message,
        })
    }

    pub fn restart(&self, g: &Group, service: &str) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["restart".into(), service.into()]);
        self.run(&g.dir, &a).map(|_| ())
    }

    /// Stop and remove the group's containers (volumes are kept).
    pub fn down(&self, g: &Group) -> Result<()> {
        let mut a = g.base_args();
        a.extend(["down".into(), "--timeout".into(), "10".into()]);
        self.run(&g.dir, &a).map(|_| ())
    }
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
        let out = self.run(Path::new("."), &a)?;
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
        let out = self.run(Path::new("."), &a)?;
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
}
