//! The environment of a native service (ADR-0014), as a documented function
//! that does not need a running daemon (ADR-0015):
//!
//! 1. the `environment` of the Compose service it replaces,
//! 2. host rewriting: for every entry of `rewrite_hosts`, `<service>:<port>`
//!    becomes the published host address; a listed service whose port is used
//!    but not published is an error, never a silent pass-through,
//! 3. the stack file's `env` for the service (wins over 1 and 2),
//! 4. devstack's own variables and the worker switches (ADR-0008).
//!
//! Steps 1-3 are what `devstack env --export dotenv` writes.
//!
//! Callers supply the Compose data as plain maps, so this crate needs neither
//! Docker nor a daemon and is tested without either.

use devstack_config::interpolate::{self, Placeholder, Resolve};
use devstack_config::model::Service;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// From the Compose service's environment.
    Compose { group: String, service: String },
    /// From the Compose service's environment, with hosts rewritten.
    Rewritten {
        group: String,
        service: String,
        original: String,
    },
    /// From the stack file's `env`.
    StackFile,
    /// devstack's own variable or a worker switch.
    Devstack,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Compose { group, service } => write!(f, "compose {group}/{service}"),
            Origin::Rewritten {
                group,
                service,
                original,
            } => write!(f, "compose {group}/{service}, rewritten from `{original}`"),
            Origin::StackFile => write!(f, "stack file"),
            Origin::Devstack => write!(f, "devstack"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    pub value: String,
    pub origin: Origin,
}

pub type Env = BTreeMap<String, Var>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvError {
    #[error(
        "{var}: `{host}:{port}` is in rewrite_hosts but port {port} of `{host}` is not published (add it to the dev overlay's `ports`)"
    )]
    NotPublished {
        var: String,
        host: String,
        port: u16,
    },
    #[error(
        "{var}: port {port} of `{host}` is published on a port Docker assigns at start; start the Compose group first (`devstack up`)"
    )]
    NotRunning {
        var: String,
        host: String,
        port: u16,
    },
    #[error("{var}: {source}")]
    Interpolation {
        var: String,
        source: interpolate::InterpError,
    },
    #[error("compose service `{group}/{service}` not found in `docker compose config`")]
    NoComposeService { group: String, service: String },
}

/// Everything known at the moment a service is started.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub repo: PathBuf,
    /// Artifact directory of the build being started; `None` for `env --export`.
    pub artifact: Option<PathBuf>,
    /// Port of the instance being started.
    pub port: Option<u16>,
    pub is_main: bool,
    /// Main-slot URL of every service with a port.
    pub service_urls: BTreeMap<String, String>,
    /// `(group, service, container port)` -> `127.0.0.1:<published>`.
    pub published: BTreeMap<(String, String, u16), String>,
    /// `${VAR}` lookups: the process environment and the group's env files, merged by the caller.
    pub vars: BTreeMap<String, String>,
    /// Ports published on an address Docker assigns at start, while not running.
    pub pending: std::collections::BTreeSet<(String, String, u16)>,
}

impl Resolve for Context {
    fn resolve(&self, p: &Placeholder) -> Result<String, String> {
        match p {
            Placeholder::Repo => Ok(self.repo.to_string_lossy().into_owned()),
            Placeholder::Artifact => self
                .artifact
                .as_ref()
                .map(|a| a.to_string_lossy().into_owned())
                .ok_or_else(|| "`{artifact}` is only known when a build is started".into()),
            Placeholder::Port => self
                .port
                .map(|p| p.to_string())
                .ok_or_else(|| "`{port}`: the service has no port".into()),
            Placeholder::IsMain => Ok(self.is_main.to_string()),
            Placeholder::ServiceUrl(s) => self
                .service_urls
                .get(s)
                .cloned()
                .ok_or_else(|| format!("`{p}`: service `{s}` has no URL")),
            Placeholder::ComposePort {
                group,
                service,
                port,
            } => self
                .published
                .get(&(group.clone(), service.clone(), *port))
                .map(|a| format!("http://{a}"))
                .ok_or_else(|| {
                    format!("`{p}`: port {port} of `{group}/{service}` is not published")
                }),
        }
    }
}

/// Compose data for one service: its environment from `config`, by group and name.
pub type ComposeEnvs = BTreeMap<(String, String), BTreeMap<String, Option<String>>>;

/// Compose the environment of `name`. `include_devstack` adds step 4.
pub fn compose(
    name: &str,
    svc: &Service,
    ctx: &Context,
    compose_envs: &ComposeEnvs,
    include_devstack: bool,
) -> Result<Env, EnvError> {
    let mut env = Env::new();

    // 1 + 2: Compose environment, hosts rewritten.
    if let Some(r) = &svc.compose {
        let key = (r.group.clone(), r.service.clone());
        let base = compose_envs
            .get(&key)
            .ok_or_else(|| EnvError::NoComposeService {
                group: r.group.clone(),
                service: r.service.clone(),
            })?;
        let hosts: Vec<(String, String)> = svc
            .rewrite_hosts
            .iter()
            .map(|h| match h.split_once('.') {
                Some((g, s)) => (g.to_string(), s.to_string()),
                None => (r.group.clone(), h.clone()),
            })
            .collect();
        for (k, v) in base {
            let Some(v) = v else { continue }; // `environment: [NAME]` without value: not set
            let rewritten = rewrite(k, v, &hosts, ctx)?;
            let origin = if rewritten != *v {
                Origin::Rewritten {
                    group: r.group.clone(),
                    service: r.service.clone(),
                    original: v.clone(),
                }
            } else {
                Origin::Compose {
                    group: r.group.clone(),
                    service: r.service.clone(),
                }
            };
            env.insert(
                k.clone(),
                Var {
                    value: rewritten,
                    origin,
                },
            );
        }
    }

    // 3: stack file.
    let lookup = |k: &str| ctx.vars.get(k).cloned();
    for (k, v) in &svc.env {
        let value =
            interpolate::expand(v, ctx, &lookup).map_err(|source| EnvError::Interpolation {
                var: k.clone(),
                source,
            })?;
        env.insert(
            k.clone(),
            Var {
                value,
                origin: Origin::StackFile,
            },
        );
    }

    // 4: devstack variables and worker switches.
    if include_devstack {
        if let Some(w) = &svc.workers {
            for (k, v) in &w.env {
                let value = interpolate::expand(v, ctx, &lookup).map_err(|source| {
                    EnvError::Interpolation {
                        var: k.clone(),
                        source,
                    }
                })?;
                env.insert(
                    k.clone(),
                    Var {
                        value,
                        origin: Origin::Devstack,
                    },
                );
            }
        }
        env.insert(
            "DEVSTACK_SERVICE".into(),
            Var {
                value: name.into(),
                origin: Origin::Devstack,
            },
        );
        env.insert(
            "DEVSTACK_SLOT".into(),
            Var {
                value: if ctx.is_main { "main" } else { "candidate" }.into(),
                origin: Origin::Devstack,
            },
        );
    }
    Ok(env)
}

/// Replace `<host>:<port>` for every listed host where it stands as a host
/// (not as part of a longer name such as `mydb:5432` for host `db`).
fn rewrite(
    var: &str,
    value: &str,
    hosts: &[(String, String)],
    ctx: &Context,
) -> Result<String, EnvError> {
    let mut out = value.to_string();
    for (group, host) in hosts {
        let mut result = String::new();
        let mut rest = out.as_str();
        while let Some(i) = find_host(rest, host) {
            let after = &rest[i + host.len() + 1..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            let port: u16 = digits.parse().unwrap_or(0);
            let key = (group.clone(), host.clone(), port);
            let addr = match ctx.published.get(&key) {
                Some(a) => a,
                None if ctx.pending.contains(&key) => {
                    return Err(EnvError::NotRunning {
                        var: var.into(),
                        host: host.clone(),
                        port,
                    });
                }
                None => {
                    return Err(EnvError::NotPublished {
                        var: var.into(),
                        host: host.clone(),
                        port,
                    });
                }
            };
            result.push_str(&rest[..i]);
            result.push_str(addr);
            rest = &after[digits.len()..];
        }
        result.push_str(rest);
        out = result;
    }
    Ok(out)
}

/// Position of `host:<digit>` where the character before `host` cannot be part of a hostname.
fn find_host(s: &str, host: &str) -> Option<usize> {
    let pat = format!("{host}:");
    let mut from = 0;
    while let Some(off) = s[from..].find(&pat) {
        let i = from + off;
        let before_ok = s[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'));
        let after_ok = s[i + pat.len()..].starts_with(|c: char| c.is_ascii_digit());
        if before_ok && after_ok {
            return Some(i);
        }
        from = i + pat.len();
    }
    None
}

// ------------------------------------------------------------------ ${VAR}

/// The values `${VAR}` resolves against, later sources winning: the stack's
/// `env_files`, the env files of the service's Compose group (a service without
/// one, or a command, has none), the process environment. Missing files are
/// skipped, so an untracked `.env` may be absent.
pub fn variables(
    stack: &devstack_config::Stack,
    service: Option<&Service>,
    repo: &std::path::Path,
) -> BTreeMap<String, String> {
    let mut files: Vec<&String> = stack.stack.env_files.iter().collect();
    if let Some(r) = service.and_then(|s| s.compose.as_ref())
        && let Some(g) = stack.compose.get(&r.group)
    {
        files.extend(&g.env_files);
    }
    let mut vars = BTreeMap::new();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(repo.join(f)) {
            vars.extend(parse_env_file(&text));
        }
    }
    vars.extend(std::env::vars());
    vars
}

// ------------------------------------------------------------------ dotenv

/// Parse an env file in Compose's format: `KEY=VALUE`, `#` comments, optional
/// `export `, single or double quotes around the value.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            v[1..v.len() - 1].to_string()
        } else {
            // An unquoted value ends at an inline comment.
            v.split(" #").next().unwrap_or(v).trim().to_string()
        };
        out.insert(k.trim().to_string(), v);
    }
    out
}

/// Write an environment as a dotenv file (ADR-0015 export). Values are always
/// double-quoted with `\`, `"`, `$` and newlines escaped.
pub fn to_dotenv(env: &Env) -> String {
    let mut s = String::from(
        "# Written by `devstack env --export dotenv`. Contains development secrets: keep it git-ignored.\n",
    );
    for (k, v) in env {
        let escaped = v
            .value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('\n', "\\n");
        s.push_str(&format!("{k}=\"{escaped}\"\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use devstack_config::model::{ComposeRef, Workers};

    fn svc(rewrite: &[&str], env: &[(&str, &str)]) -> Service {
        let toml = "run = \"x\"\n";
        let mut s: Service = devstack_config::from_str(
            &format!("version = 1\n[stack]\nname = \"t\"\n[services.api]\n{toml}"),
            std::path::Path::new("t"),
        )
        .unwrap()
        .services
        .remove("api")
        .unwrap();
        s.compose = Some(ComposeRef {
            group: "app".into(),
            service: "api".into(),
        });
        s.rewrite_hosts = rewrite.iter().map(|s| s.to_string()).collect();
        s.env = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    fn ctx() -> Context {
        let mut c = Context {
            repo: "/repo".into(),
            port: Some(5000),
            is_main: true,
            ..Default::default()
        };
        c.published
            .insert(("app".into(), "db".into(), 5432), "127.0.0.1:15432".into());
        c.published.insert(
            ("search".into(), "es".into(), 9200),
            "127.0.0.1:19200".into(),
        );
        c.service_urls
            .insert("mock".into(), "http://127.0.0.1:5310".into());
        c.vars.insert("SECRET".into(), "s3cr3t".into());
        c
    }

    fn compose_env(pairs: &[(&str, Option<&str>)]) -> ComposeEnvs {
        let mut m = ComposeEnvs::new();
        m.insert(
            ("app".into(), "api".into()),
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
                .collect(),
        );
        m
    }

    #[test]
    fn layers_and_origins() {
        let ce = compose_env(&[
            ("DB", Some("Server=db:5432;Database=x")),
            ("ES", Some("http://es:9200")),
            ("KEEP", Some("http://mydb:5432")),
            ("OVERRIDE", Some("from-compose")),
            ("UNSET", None),
        ]);
        let s = svc(
            &["db", "search.es"],
            &[
                ("OVERRIDE", "{service.mock.url}/v1"),
                ("PASS", "${SECRET}"),
                ("DEF", "${NOPE:-{port}}"),
            ],
        );
        let env = compose("api", &s, &ctx(), &ce, false).unwrap();
        assert_eq!(env["DB"].value, "Server=127.0.0.1:15432;Database=x");
        assert!(matches!(env["DB"].origin, Origin::Rewritten { .. }));
        assert_eq!(env["ES"].value, "http://127.0.0.1:19200");
        assert_eq!(
            env["KEEP"].value, "http://mydb:5432",
            "a longer host name must not be rewritten"
        );
        assert_eq!(
            env["KEEP"].origin,
            Origin::Compose {
                group: "app".into(),
                service: "api".into()
            }
        );
        assert_eq!(env["OVERRIDE"].value, "http://127.0.0.1:5310/v1");
        assert_eq!(env["OVERRIDE"].origin, Origin::StackFile);
        assert_eq!(env["PASS"].value, "s3cr3t");
        assert_eq!(env["DEF"].value, "5000");
        assert!(!env.contains_key("UNSET"));
        assert!(!env.contains_key("DEVSTACK_SLOT"));
    }

    #[test]
    fn listed_host_with_unpublished_port_is_an_error() {
        let ce = compose_env(&[("DB", Some("db:6379"))]);
        let err = compose("api", &svc(&["db"], &[]), &ctx(), &ce, false).unwrap_err();
        assert_eq!(
            err,
            EnvError::NotPublished {
                var: "DB".into(),
                host: "db".into(),
                port: 6379
            }
        );
    }

    #[test]
    fn devstack_variables_and_workers() {
        let mut s = svc(&[], &[]);
        s.workers = Some(Workers {
            env: [("Jobs__Enabled".to_string(), "{is_main}".to_string())].into(),
        });
        let mut c = ctx();
        c.is_main = false;
        let env = compose("api", &s, &c, &compose_env(&[]), true).unwrap();
        assert_eq!(env["Jobs__Enabled"].value, "false");
        assert_eq!(env["DEVSTACK_SLOT"].value, "candidate");
        assert_eq!(env["DEVSTACK_SERVICE"].value, "api");
    }

    #[test]
    fn dotenv_round_trip() {
        let text = "# c\nA=1\nexport B=\"two words\"\nC='x=y'\nD=plain # note\n";
        let m = parse_env_file(text);
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "two words");
        assert_eq!(m["C"], "x=y");
        assert_eq!(m["D"], "plain");
        let mut env = Env::new();
        env.insert(
            "Q".into(),
            Var {
                value: "a\"b$c".into(),
                origin: Origin::StackFile,
            },
        );
        assert!(to_dotenv(&env).contains("Q=\"a\\\"b\\$c\""));
    }

    #[test]
    fn variables_from_stack_files_then_group_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stack.env"), "A=stack\nB=stack\n").unwrap();
        std::fs::write(dir.path().join("group.env"), "B=group\n").unwrap();
        let toml = r#"
version = 1
[stack]
name = "t"
env_files = ["stack.env", "missing.env"]
[compose.g]
project = "p"
files = ["c.yml"]
env_files = ["group.env"]
services = ["db"]
[services.linked]
compose = { group = "g", service = "x" }
run = "x"
[services.plain]
run = "x"
"#;
        let stack = devstack_config::from_str(toml, &dir.path().join("devstack.toml")).unwrap();
        let linked = variables(&stack, Some(&stack.services["linked"]), dir.path());
        assert_eq!(
            (linked["A"].as_str(), linked["B"].as_str()),
            ("stack", "group")
        );
        let plain = variables(&stack, Some(&stack.services["plain"]), dir.path());
        assert_eq!(
            (plain["A"].as_str(), plain["B"].as_str()),
            ("stack", "stack")
        );
        let command = variables(&stack, None, dir.path());
        assert_eq!(command["B"], "stack");
    }
}
