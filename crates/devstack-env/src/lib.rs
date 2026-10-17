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
    /// `{is_main}`: the instance runs the background workers (ADR-0008).
    pub is_main: bool,
    /// The instance starts beside a running one (ADR-0003):
    /// `DEVSTACK_SLOT=candidate` instead of `main`. Not the same as
    /// `!is_main`: a lane's main instance runs no workers, but is no candidate.
    pub candidate: bool,
    /// The lane (plan §3.5); empty means `main`.
    pub lane: String,
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
            Placeholder::Lane => Ok(lane_name(&self.lane).to_string()),
            Placeholder::LaneSuffix => Ok(lane_suffix(&self.lane)),
            Placeholder::ServiceUrl(s) => self
                .service_urls
                .get(s)
                .cloned()
                .ok_or_else(|| format!("`{p}`: service `{s}` has no URL")),
            Placeholder::ServicePort(s) => self
                .service_urls
                .get(s)
                .and_then(|u| u.rsplit(':').next())
                .map(str::to_string)
                .ok_or_else(|| format!("`{p}`: service `{s}` has no port")),
            Placeholder::ComposePort {
                group,
                service,
                port,
                part,
            } => self
                .published
                .get(&(group.clone(), service.clone(), *port))
                .map(|a| part.of(a))
                .ok_or_else(|| {
                    format!("`{p}`: port {port} of `{group}/{service}` is not published")
                }),
        }
    }
}

impl Context {
    /// A `${VAR}` lookup: case-insensitive on Windows, as the environment
    /// there is (`${PATH}` finds `Path`).
    pub fn var(&self, name: &str) -> Option<String> {
        lookup(&self.vars, name).cloned()
    }
}

/// `map[name]`, on Windows without regard to case when there is no exact match.
pub fn lookup<'a, T>(map: &'a BTreeMap<String, T>, name: &str) -> Option<&'a T> {
    map.get(name).or_else(|| {
        if cfg!(windows) {
            map.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        } else {
            None
        }
    })
}

/// Insert `name`, replacing on Windows an entry that differs only in case:
/// a process gets one of them, and the later layer has to be the one.
pub fn insert<T>(map: &mut BTreeMap<String, T>, name: String, value: T) {
    if cfg!(windows)
        && let Some(old) = map
            .keys()
            .find(|k| **k != name && k.eq_ignore_ascii_case(&name))
            .cloned()
    {
        map.remove(&old);
    }
    map.insert(name, value);
}

/// `main` for the main working tree.
pub fn lane_name(lane: &str) -> &str {
    if lane.is_empty() { "main" } else { lane }
}

/// Empty in the main lane, `_<lane>` in another one.
pub fn lane_suffix(lane: &str) -> String {
    match lane_name(lane) {
        "main" => String::new(),
        l => format!("_{l}"),
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

    // 1 + 2: Compose environment, hosts rewritten (not for a reference-only link).
    if let Some(r) = svc.compose.as_ref().filter(|r| r.inherit) {
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
            insert(
                &mut env,
                k.clone(),
                Var {
                    value: rewritten,
                    origin,
                },
            );
        }
    }

    // 3: stack file.
    let lookup = |k: &str| ctx.var(k);
    for (k, v) in &svc.env {
        let value =
            interpolate::expand(v, ctx, &lookup).map_err(|source| EnvError::Interpolation {
                var: k.clone(),
                source,
            })?;
        insert(
            &mut env,
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
                insert(
                    &mut env,
                    k.clone(),
                    Var {
                        value,
                        origin: Origin::Devstack,
                    },
                );
            }
        }
        insert(
            &mut env,
            "DEVSTACK_SERVICE".into(),
            Var {
                value: name.into(),
                origin: Origin::Devstack,
            },
        );
        insert(
            &mut env,
            "DEVSTACK_SLOT".into(),
            Var {
                value: if ctx.candidate { "candidate" } else { "main" }.into(),
                origin: Origin::Devstack,
            },
        );
        insert(
            &mut env,
            "DEVSTACK_LANE".into(),
            Var {
                value: lane_name(&ctx.lane).into(),
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
            let Ok(port) = digits.parse::<u16>() else {
                // `db:99999` is no port: left as it is, never "port 0".
                let end = i + host.len() + 1 + digits.len();
                result.push_str(&rest[..end]);
                rest = &rest[end..];
                continue;
            };
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

// ------------------------------------------------------------------ drift

/// For a reference-only Compose link (`inherit = false`): the variables of the
/// Compose service that the stack file neither sets (`env`, worker switches)
/// nor ignores. Sorted. Empty for an inheriting link.
pub fn drift<'a>(svc: &Service, compose_keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let Some(r) = svc.compose.as_ref().filter(|r| !r.inherit) else {
        return Vec::new();
    };
    let set = |k: &str| {
        svc.env.contains_key(k) || svc.workers.as_ref().is_some_and(|w| w.env.contains_key(k))
    };
    let mut out: Vec<String> = compose_keys
        .into_iter()
        .filter(|k| !set(k) && !r.ignore.iter().any(|p| glob(p, k)))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `*` matches any run of characters; everything else literally.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if text.len() < first.len() + last.len() || !text.starts_with(first) || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
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
            // `${VAR}` in a file sees the process environment, then what the
            // files before it set.
            let parsed =
                parse_env_file_with(&text, &|k| env_var(k).or_else(|| lookup(&vars, k).cloned()));
            for (k, v) in parsed {
                insert(&mut vars, k, v);
            }
        }
    }
    for (k, v) in std::env::vars() {
        insert(&mut vars, k, v);
    }
    vars
}

/// A variable of the process environment (case-insensitive on Windows, by
/// the platform itself).
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

// ------------------------------------------------------------------ dotenv

/// Parse an env file in Compose's dialect:
///
/// - `KEY=VALUE` lines, `#` comment lines, an optional `export `;
/// - unquoted: up to the end of the line or an inline ` #` comment, `${VAR}`
///   expanded;
/// - `"double quoted"`: may span lines; `\n`, `\r`, `\t`, `\\`, `\"` and `\$`
///   are escapes, `$$` is a `$`, `${VAR}` is expanded; a `#` inside is text,
///   one after the closing quote a comment;
/// - `'single quoted'`: taken as written, may span lines.
///
/// `${VAR}`, `${VAR:-default}`, `${VAR-default}` and `$VAR` look in the
/// process environment first, then in the variables defined above them; an
/// unknown one is empty, as in Compose.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    parse_env_file_with(text, &env_var)
}

/// [`parse_env_file`] with `${VAR}` looked up in `outer` before the
/// variables defined above it in the file.
pub fn parse_env_file_with(
    text: &str,
    outer: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
        let stmt = line.trim();
        let stmt = stmt.strip_prefix("export ").map_or(stmt, str::trim_start);
        let Some((key, value)) = stmt.split_once('=').filter(|_| !stmt.starts_with('#')) else {
            rest = after;
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
        // Where the value starts within `rest`: a quoted one may run past this line.
        let start = value.as_ptr() as usize - rest.as_ptr() as usize;
        let lookup = |k: &str| outer(k).or_else(|| lookup(&out, k).cloned());
        let parsed = match quote {
            Some(q) => closing_quote(&rest[start + 1..], q).map(|end| {
                let raw = &rest[start + 1..start + 1 + end];
                let value = if q == '"' {
                    expand_value(raw, true, &lookup)
                } else {
                    raw.to_string()
                };
                // Whatever follows the closing quote on its line is a comment.
                let tail = &rest[start + 1 + end + 1..];
                let next = tail.split_once('\n').map_or("", |(_, n)| n);
                (value, next)
            }),
            None => None,
        };
        let (value, next) = parsed.unwrap_or_else(|| {
            // Unquoted, or a quote that never closes: up to an inline comment.
            let v = value
                .split_once(" #")
                .or_else(|| value.split_once("\t#"))
                .map_or(value, |(v, _)| v)
                .trim_end();
            (expand_value(v, false, &lookup), after)
        });
        insert(&mut out, key, value);
        rest = next;
    }
    out
}

/// Byte offset of the quote that closes a value opened with `q`; a
/// double-quoted value skips escaped characters.
fn closing_quote(s: &str, q: char) -> Option<usize> {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' && q == '"' {
            chars.next();
        } else if c == q {
            return Some(i);
        }
    }
    None
}

/// Escapes (double quotes only) and variable references of a value.
fn expand_value(s: &str, escapes: bool, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' if escapes && i + 1 < c.len() => {
                match c[i + 1] {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    '\\' | '"' | '$' => out.push(c[i + 1]),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                i += 2;
            }
            '$' if c.get(i + 1) == Some(&'$') => {
                out.push('$');
                i += 2;
            }
            '$' if c.get(i + 1) == Some(&'{') => {
                let Some(close) = c[i + 2..].iter().position(|&x| x == '}') else {
                    out.extend(&c[i..]);
                    break;
                };
                let body: String = c[i + 2..i + 2 + close].iter().collect();
                let name_len = body
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .unwrap_or(body.len());
                let (name, op) = body.split_at(name_len);
                let value = lookup(name);
                let value = if let Some(d) = op.strip_prefix(":-") {
                    value
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| expand_value(d, false, lookup))
                } else if let Some(d) = op.strip_prefix('-') {
                    value.unwrap_or_else(|| expand_value(d, false, lookup))
                } else {
                    // `${VAR}`, and `${VAR:?message}`, which only Compose reports.
                    value.unwrap_or_default()
                };
                out.push_str(&value);
                i += 2 + close + 1;
            }
            '$' if c
                .get(i + 1)
                .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == '_') =>
            {
                let len = c[i + 1..]
                    .iter()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || **ch == '_')
                    .count();
                let name: String = c[i + 1..i + 1 + len].iter().collect();
                out.push_str(&lookup(&name).unwrap_or_default());
                i += 1 + len;
            }
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// Write an environment as a dotenv file (ADR-0015 export) in Compose's
/// dialect, which [`parse_env_file`] reads back: every value double-quoted,
/// with `\`, `"`, line breaks escaped and `$` written `$$`, so nothing in it
/// is expanded on the way in.
pub fn to_dotenv(env: &Env) -> String {
    let mut s = String::from(
        "# Written by `devstack env --export dotenv`. Contains development secrets: keep it git-ignored.\n",
    );
    for (k, v) in env {
        let escaped = v
            .value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
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
            inherit: true,
            ignore: Vec::new(),
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
        c.candidate = true;
        let env = compose("api", &s, &c, &compose_env(&[]), true).unwrap();
        assert_eq!(env["Jobs__Enabled"].value, "false");
        assert_eq!(env["DEVSTACK_SLOT"].value, "candidate");
        assert_eq!(env["DEVSTACK_SERVICE"].value, "api");
        assert_eq!(env["DEVSTACK_LANE"].value, "main");

        // A lane's main instance: no workers, but no candidate either (F-23).
        let mut c = ctx();
        c.is_main = false;
        c.lane = "feat-x".into();
        let env = compose("api", &s, &c, &compose_env(&[]), true).unwrap();
        assert_eq!(env["Jobs__Enabled"].value, "false");
        assert_eq!(env["DEVSTACK_SLOT"].value, "main");
        assert_eq!(env["DEVSTACK_LANE"].value, "feat-x");
    }

    #[test]
    fn env_files_in_composes_dialect() {
        let text = concat!(
            "# c\n",
            "A=1\n",
            "export B=\"two words\"\n",
            "C='x=y'\n",
            "D=plain # note\n",
            "E=\"quoted\" # note\n",
            "F='single' # note\n",
            "G=\"has # hash\"\n",
            "H=\"line1\\nline2\\t\\\"q\\\" \\\\ \\$HOME $$x\"\n",
            "I=\"multi\n",
            "line\"\n",
            "J=${A}-$A-${NOPE:-dflt}-${NOPE-d2}-${EMPTY:-e}-${EMPTY-kept}\n",
            "EMPTY=\n",
            "K=${EMPTY:-e}${EMPTY-kept}\n",
            "L='${A} stays'\n",
            "M=\"${A}x\"\n",
            "N=\"never closed\n",
            "O=ok\r\n",
        );
        let m = parse_env_file_with(text, &|_| None);
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "two words");
        assert_eq!(m["C"], "x=y");
        assert_eq!(m["D"], "plain");
        assert_eq!(m["E"], "quoted");
        assert_eq!(m["F"], "single");
        assert_eq!(m["G"], "has # hash");
        assert_eq!(m["H"], "line1\nline2\t\"q\" \\ $HOME $x");
        assert_eq!(m["I"], "multi\nline");
        assert_eq!(m["J"], "1-1-dflt-d2-e-kept");
        assert_eq!(m["K"], "e");
        assert_eq!(m["L"], "${A} stays");
        assert_eq!(m["M"], "1x");
        assert_eq!(m["N"], "\"never closed");
        assert_eq!(m["O"], "ok");
        // The process environment comes before the file's own variables.
        let m = parse_env_file_with("A=file\nB=${A}\n", &|k| (k == "A").then(|| "env".into()));
        assert_eq!(m["B"], "env");
    }

    /// F-22: what `--export dotenv` writes reads back as it was.
    #[test]
    fn dotenv_round_trip() {
        let values = [
            "plain",
            "a\"b$c",
            "pa$$word${HOME}$HOME",
            "line1\nline2\r\n",
            "back\\slash \\n \\$ \\\"",
            "trailing\\",
            "# not a comment",
            "'single' and \"double\"",
            "",
            "ünïcödé",
            "x = y # z",
        ];
        let mut env = Env::new();
        for (i, v) in values.iter().enumerate() {
            env.insert(
                format!("V{i}"),
                Var {
                    value: v.to_string(),
                    origin: Origin::StackFile,
                },
            );
        }
        let text = to_dotenv(&env);
        assert!(text.contains("V1=\"a\\\"b$$c\""), "{text}");
        let back = parse_env_file_with(&text, &|_| Some("WRONG".into()));
        for (i, v) in values.iter().enumerate() {
            assert_eq!(back[&format!("V{i}")], *v, "{text}");
        }
        assert_eq!(back.len(), values.len());
    }

    #[test]
    fn a_number_too_big_for_a_port_is_left_alone() {
        let ce = compose_env(&[("DB", Some("db:99999 and db:5432"))]);
        let env = compose("api", &svc(&["db"], &[]), &ctx(), &ce, false).unwrap();
        assert_eq!(env["DB"].value, "db:99999 and 127.0.0.1:15432");
    }

    #[cfg(windows)]
    #[test]
    fn names_differing_in_case_are_one_variable_on_windows() {
        let ce = compose_env(&[("Path", Some("from-compose"))]);
        let s = svc(&[], &[("PATH", "${path_var}")]);
        let mut c = ctx();
        c.vars.insert("PATH_VAR".into(), "from-vars".into());
        let env = compose("api", &s, &c, &ce, false).unwrap();
        assert!(!env.contains_key("Path"), "{env:?}");
        assert_eq!(env["PATH"].value, "from-vars");
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

    #[test]
    fn glob_patterns() {
        assert!(glob("Mail__*", "Mail__Send__X"));
        assert!(glob("*__StorageId", "Orders__Mail__StorageId"));
        assert!(glob("A*B*C", "AxxByyC"));
        assert!(!glob("A*B*C", "AxxC"));
        assert!(!glob("AB*BA", "ABA"), "no overlap of prefix and suffix");
        assert!(glob("TZ", "TZ") && !glob("TZ", "TZX"));
    }

    #[test]
    fn drift_lists_what_a_reference_link_leaves_unset() {
        let dir = tempfile::tempdir().unwrap();
        let toml = r#"
version = 1
[stack]
name = "t"
[compose.g]
project = "p"
files = ["c.yml"]
services = ["db"]
[services.api]
compose = { group = "g", service = "api", inherit = false, ignore = ["Mail__*", "TZ"] }
run = "x"
env = { Db = "x" }
workers = { env = { Jobs = "{is_main}" } }
[services.full]
compose = { group = "g", service = "api" }
run = "x"
"#;
        let stack = devstack_config::from_str(toml, &dir.path().join("devstack.toml")).unwrap();
        let keys = ["Db", "Jobs", "Mail__Send", "TZ", "NewSetting", "Other"];
        assert_eq!(
            drift(&stack.services["api"], keys),
            vec!["NewSetting".to_string(), "Other".to_string()]
        );
        assert!(
            drift(&stack.services["full"], keys).is_empty(),
            "inheriting links have no drift"
        );
    }
}
