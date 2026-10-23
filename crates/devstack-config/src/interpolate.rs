//! Interpolation (plan §4.5): a closed set of `{…}` placeholders and `${VAR}` /
//! `${VAR:-default}` environment references. Placeholders inside a default are
//! resolved too (`"${AI_URL:-{service.ai-mock.url}/v1}"`), which is the plan's
//! "`{…}` first, then `${…}`". `{{` and `}}` are literal braces, inside a
//! default too (where a single `}` ends the default).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placeholder {
    /// `{repo}`: absolute path of the lane's source root.
    Repo,
    /// `{artifact}`: artifact directory of the build being started.
    Artifact,
    /// `{port}`: port of the instance being started.
    Port,
    /// `{is_main}`: `true` in the main slot, `false` otherwise.
    IsMain,
    /// `{lane}`: name of the lane (plan §3.5), `main` for the main working tree.
    Lane,
    /// `{lane_suffix}`: empty in the main lane, `_<lane>` in another one: for
    /// per-lane resources that keep their name in the main lane (`app{lane_suffix}`).
    LaneSuffix,
    /// `{service.<name>.url}`: URL of that service's main slot.
    ServiceUrl(String),
    /// `{service.<name>.port}`: the port of that service's main slot.
    ServicePort(String),
    /// `{compose.<group>.<service>.<port>[.host|.port|.address]}`: where a
    /// container port is published on this machine: a URL by default, or its
    /// host, its port, or `host:port` (connection strings want them apart:
    /// `Server=host,port`, `Host=host;Port=port`).
    ComposePort {
        group: String,
        service: String,
        port: u16,
        part: AddrPart,
    },
    /// `{compose.<group>.project}`: the group's Compose project as this tree
    /// runs it: `<project>` in the main tree, the lane's own in a lane (for
    /// `docker compose -p {compose.app.project} exec ...`).
    ComposeProject(String),
}

/// Which part of a published address a `{compose.…}` placeholder gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddrPart {
    /// `http://127.0.0.1:15432`
    #[default]
    Url,
    /// `127.0.0.1`
    Host,
    /// `15432`
    Port,
    /// `127.0.0.1:15432`
    Address,
}

impl AddrPart {
    /// This part of a `host:port` address.
    pub fn of(self, address: &str) -> String {
        let (host, port) = address.rsplit_once(':').unwrap_or((address, ""));
        match self {
            AddrPart::Url => format!("http://{address}"),
            AddrPart::Host => host.to_string(),
            AddrPart::Port => port.to_string(),
            AddrPart::Address => address.to_string(),
        }
    }
}

impl fmt::Display for Placeholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Placeholder::Repo => write!(f, "{{repo}}"),
            Placeholder::Artifact => write!(f, "{{artifact}}"),
            Placeholder::Port => write!(f, "{{port}}"),
            Placeholder::IsMain => write!(f, "{{is_main}}"),
            Placeholder::Lane => write!(f, "{{lane}}"),
            Placeholder::LaneSuffix => write!(f, "{{lane_suffix}}"),
            Placeholder::ServiceUrl(s) => write!(f, "{{service.{s}.url}}"),
            Placeholder::ServicePort(s) => write!(f, "{{service.{s}.port}}"),
            Placeholder::ComposePort {
                group,
                service,
                port,
                part,
            } => {
                let suffix = match part {
                    AddrPart::Url => "",
                    AddrPart::Host => ".host",
                    AddrPart::Port => ".port",
                    AddrPart::Address => ".address",
                };
                write!(f, "{{compose.{group}.{service}.{port}{suffix}}}")
            }
            Placeholder::ComposeProject(g) => write!(f, "{{compose.{g}.project}}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    Placeholder(Placeholder),
    Env {
        name: String,
        default: Option<Vec<Segment>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InterpError {
    #[error(
        "unknown placeholder `{{{0}}}` (allowed: repo, artifact, port, is_main, lane, lane_suffix, service.<name>.url, service.<name>.port, compose.<group>.project, compose.<group>.<service>.<port> with an optional .host, .port or .address)"
    )]
    UnknownPlaceholder(String),
    #[error("unclosed `{0}`")]
    Unclosed(&'static str),
    #[error("unmatched `}}` (write `}}}}` for a literal brace)")]
    Unmatched,
    #[error("invalid variable name in `${{{0}}}`")]
    BadVarName(String),
    #[error("environment variable `{0}` is not set and has no default")]
    MissingVar(String),
    #[error("{0}")]
    Unresolved(String),
}

/// Parse a string into segments. Errors are syntax errors only.
pub fn parse(input: &str) -> Result<Vec<Segment>, InterpError> {
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0;
    let segs = parse_until(&chars, &mut pos, false)?;
    Ok(segs)
}

fn push_lit(out: &mut Vec<Segment>, c: char) {
    if let Some(Segment::Literal(s)) = out.last_mut() {
        s.push(c);
    } else {
        out.push(Segment::Literal(c.to_string()));
    }
}

fn parse_until(c: &[char], pos: &mut usize, in_default: bool) -> Result<Vec<Segment>, InterpError> {
    let mut out = Vec::new();
    while *pos < c.len() {
        match c[*pos] {
            '{' if c.get(*pos + 1) == Some(&'{') => {
                push_lit(&mut out, '{');
                *pos += 2;
            }
            // Inside a default, `}}` is a literal brace too; a single one closes it.
            '}' if in_default && c.get(*pos + 1) == Some(&'}') => {
                push_lit(&mut out, '}');
                *pos += 2;
            }
            '}' if in_default => {
                return Ok(out); // caller consumes the closing brace
            }
            '}' if c.get(*pos + 1) == Some(&'}') => {
                push_lit(&mut out, '}');
                *pos += 2;
            }
            '}' => return Err(InterpError::Unmatched),
            '$' if c.get(*pos + 1) == Some(&'{') => {
                *pos += 2;
                let start = *pos;
                while *pos < c.len() && (c[*pos].is_ascii_alphanumeric() || c[*pos] == '_') {
                    *pos += 1;
                }
                let name: String = c[start..*pos].iter().collect();
                if name.is_empty() || name.starts_with(|ch: char| ch.is_ascii_digit()) {
                    let rest: String = c[start..].iter().take_while(|&&ch| ch != '}').collect();
                    return Err(InterpError::BadVarName(rest));
                }
                let default = if c.get(*pos) == Some(&':') && c.get(*pos + 1) == Some(&'-') {
                    *pos += 2;
                    Some(parse_until(c, pos, true)?)
                } else {
                    None
                };
                if c.get(*pos) != Some(&'}') {
                    return Err(InterpError::Unclosed("${"));
                }
                *pos += 1;
                out.push(Segment::Env { name, default });
            }
            '{' => {
                *pos += 1;
                let start = *pos;
                while *pos < c.len() && c[*pos] != '}' {
                    if c[*pos] == '{' {
                        return Err(InterpError::Unclosed("{"));
                    }
                    *pos += 1;
                }
                if *pos >= c.len() {
                    return Err(InterpError::Unclosed("{"));
                }
                let body: String = c[start..*pos].iter().collect();
                *pos += 1;
                out.push(Segment::Placeholder(placeholder(&body)?));
            }
            ch => {
                push_lit(&mut out, ch);
                *pos += 1;
            }
        }
    }
    if in_default {
        return Err(InterpError::Unclosed("${"));
    }
    Ok(out)
}

fn placeholder(body: &str) -> Result<Placeholder, InterpError> {
    let parts: Vec<&str> = body.split('.').collect();
    let unknown = || InterpError::UnknownPlaceholder(body.to_string());
    match parts.as_slice() {
        ["repo"] => Ok(Placeholder::Repo),
        ["artifact"] => Ok(Placeholder::Artifact),
        ["port"] => Ok(Placeholder::Port),
        ["is_main"] => Ok(Placeholder::IsMain),
        ["lane"] => Ok(Placeholder::Lane),
        ["lane_suffix"] => Ok(Placeholder::LaneSuffix),
        ["service", name, "url"] if !name.is_empty() => {
            Ok(Placeholder::ServiceUrl(name.to_string()))
        }
        ["service", name, "port"] if !name.is_empty() => {
            Ok(Placeholder::ServicePort(name.to_string()))
        }
        ["compose", group, "project"] if !group.is_empty() => {
            Ok(Placeholder::ComposeProject(group.to_string()))
        }
        ["compose", group, service, port, rest @ ..]
            if !group.is_empty() && !service.is_empty() && rest.len() <= 1 =>
        {
            let part = match rest.first() {
                None => AddrPart::Url,
                Some(&"host") => AddrPart::Host,
                Some(&"port") => AddrPart::Port,
                Some(&"address") => AddrPart::Address,
                Some(_) => return Err(unknown()),
            };
            Ok(Placeholder::ComposePort {
                group: group.to_string(),
                service: service.to_string(),
                port: port.parse().map_err(|_| unknown())?,
                part,
            })
        }
        _ => Err(unknown()),
    }
}

/// Every placeholder in a parsed string, including those inside defaults.
pub fn placeholders(segs: &[Segment]) -> Vec<&Placeholder> {
    let mut out = Vec::new();
    for s in segs {
        match s {
            Segment::Placeholder(p) => out.push(p),
            Segment::Env {
                default: Some(d), ..
            } => out.extend(placeholders(d)),
            _ => {}
        }
    }
    out
}

/// Supplies the values of placeholders at the moment a string is expanded.
pub trait Resolve {
    fn resolve(&self, p: &Placeholder) -> Result<String, String>;
}

/// Expand a string. `env` looks variables up (process environment and the
/// Compose group's env files, in the caller's order).
pub fn expand(
    input: &str,
    r: &dyn Resolve,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, InterpError> {
    let segs = parse(input)?;
    eval(&segs, r, env)
}

fn eval(
    segs: &[Segment],
    r: &dyn Resolve,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, InterpError> {
    let mut out = String::new();
    for s in segs {
        match s {
            Segment::Literal(l) => out.push_str(l),
            Segment::Placeholder(p) => {
                out.push_str(&r.resolve(p).map_err(InterpError::Unresolved)?)
            }
            Segment::Env { name, default } => match (env(name), default) {
                (Some(v), _) => out.push_str(&v),
                (None, Some(d)) => out.push_str(&eval(d, r, env)?),
                (None, None) => return Err(InterpError::MissingVar(name.clone())),
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct R;
    impl Resolve for R {
        fn resolve(&self, p: &Placeholder) -> Result<String, String> {
            Ok(match p {
                Placeholder::Repo => "/repo".into(),
                Placeholder::Artifact => "/art".into(),
                Placeholder::Port => "5000".into(),
                Placeholder::IsMain => "true".into(),
                Placeholder::Lane => "feat-x".into(),
                Placeholder::LaneSuffix => "_feat-x".into(),
                Placeholder::ServiceUrl(s) => format!("http://127.0.0.1:1/{s}"),
                Placeholder::ServicePort(_) => "1".into(),
                Placeholder::ComposeProject(g) => format!("shop-{g}-feat-x"),
                Placeholder::ComposePort {
                    service,
                    port,
                    part,
                    ..
                } => match part {
                    AddrPart::Url => format!("http://127.0.0.1:9{port}/{service}"),
                    other => other.of(&format!("127.0.0.1:9{port}")),
                },
            })
        }
    }
    fn env(k: &str) -> Option<String> {
        (k == "SET").then(|| "value".to_string())
    }
    fn x(s: &str) -> Result<String, InterpError> {
        expand(s, &R, &env)
    }

    #[test]
    fn placeholders_and_literals() {
        assert_eq!(x("{repo}/dev/{port}").unwrap(), "/repo/dev/5000");
        assert_eq!(
            x("{service.api.url}/v1").unwrap(),
            "http://127.0.0.1:1/api/v1"
        );
        assert_eq!(
            x("{compose.app.hooks.8080}/x").unwrap(),
            "http://127.0.0.1:98080/hooks/x"
        );
        assert_eq!(x("plain").unwrap(), "plain");
        assert_eq!(
            x("app{lane_suffix} in {lane}").unwrap(),
            "app_feat-x in feat-x"
        );
    }

    #[test]
    fn env_refs_and_nested_defaults() {
        assert_eq!(x("${SET}").unwrap(), "value");
        assert_eq!(x("${UNSET:-fallback}").unwrap(), "fallback");
        assert_eq!(x("${UNSET:-}").unwrap(), "");
        assert_eq!(
            x("${UNSET:-{service.ai.url}/v1}").unwrap(),
            "http://127.0.0.1:1/ai/v1"
        );
        assert_eq!(x("${SET:-{service.ai.url}}").unwrap(), "value");
        assert_eq!(x("${UNSET}"), Err(InterpError::MissingVar("UNSET".into())));
    }

    #[test]
    fn escapes() {
        assert_eq!(x("{{\"json\": 1}}").unwrap(), "{\"json\": 1}");
        assert_eq!(x("$HOME and $ alone").unwrap(), "$HOME and $ alone");
    }

    #[test]
    fn a_default_can_hold_a_literal_brace() {
        assert_eq!(x("${UNSET:-{{\"a\": 1}}}").unwrap(), "{\"a\": 1}");
        assert_eq!(x("${SET:-x}}}").unwrap(), "value");
        assert_eq!(x("${UNSET:-x}}}").unwrap(), "x}");
    }

    #[test]
    fn syntax_errors() {
        assert!(matches!(
            x("{nope}"),
            Err(InterpError::UnknownPlaceholder(_))
        ));
        assert!(matches!(
            x("{service.api}"),
            Err(InterpError::UnknownPlaceholder(_))
        ));
        assert!(matches!(
            x("{compose.a.b.port}"),
            Err(InterpError::UnknownPlaceholder(_))
        ));
        assert_eq!(x("{repo"), Err(InterpError::Unclosed("{")));
        assert_eq!(x("${A:-x"), Err(InterpError::Unclosed("${")));
        assert_eq!(x("a}b"), Err(InterpError::Unmatched));
        assert!(matches!(x("${1A}"), Err(InterpError::BadVarName(_))));
    }

    #[test]
    fn collects_nested_placeholders() {
        let segs = parse("${A:-{service.x.url}}{port}").unwrap();
        let p = placeholders(&segs);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn compose_addresses_come_whole_or_in_parts() {
        let get = |s: &str| expand(s, &R, &env).unwrap();
        assert_eq!(get("{compose.app.db.5432}"), "http://127.0.0.1:95432/db");
        assert_eq!(
            get("Server={compose.app.db.1433.host},{compose.app.db.1433.port}"),
            "Server=127.0.0.1,91433"
        );
        assert_eq!(get("{compose.app.db.5432.address}"), "127.0.0.1:95432");
        assert_eq!(get("{service.api.port}"), "1");
        assert_eq!(
            get("docker compose -p {compose.app.project} exec -T db"),
            "docker compose -p shop-app-feat-x exec -T db"
        );
        assert_eq!(
            placeholder("compose.app.project").unwrap(),
            Placeholder::ComposeProject("app".into())
        );
        assert_eq!(
            placeholder("compose.app.project").unwrap().to_string(),
            "{compose.app.project}"
        );
        assert!(parse("{compose..project}").is_err());
        assert!(parse("{compose.app.db.5432.scheme}").is_err());
        assert_eq!(
            placeholder("compose.app.db.5432.port").unwrap().to_string(),
            "{compose.app.db.5432.port}"
        );
    }
}
