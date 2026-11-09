//! The MCP servers a coding agent has configured itself, outside devstack
//! (#473): a member who followed a service's own setup hint (`claude mcp add
//! … https://tracker.example/mcp`) has a pool's upstream twice over, or has
//! it without having accepted it in the hub, and a hook bound to that
//! upstream (ADR-0094) stayed silent.
//!
//! Read only, and only each server's name and URL: no header, no token, no
//! command line is kept or logged, and nothing is written (card point 4,
//! 2026-11-06). Matched to a pool's upstream by its URL, not by its name.
//!
//! - **Claude Code:** `.claude.json` in `CLAUDE_CONFIG_DIR`, else in the
//!   home folder: `mcpServers` (user scope) and `projects.<folder>.mcpServers`
//!   (local scope, that folder only); `.mcp.json` in a working tree (project
//!   scope).
//! - **Codex:** `config.toml` in `CODEX_HOME`, else `~/.codex`:
//!   `[mcp_servers.<name>]` with a `url`.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Where Claude Code keeps its own configuration.
const CLAUDE_FILE: &str = "${CLAUDE_CONFIG_DIR:-~}/.claude.json";
/// Where Codex keeps its own configuration.
const CODEX_FILE: &str = "${CODEX_HOME:-~/.codex}/config.toml";

/// Where an agent's own entry is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// For the user, in every folder.
    User,
    /// For the user, in one folder (`project`).
    Local,
    /// In a working tree's `.mcp.json`, for everyone who checks it out.
    Project,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Local => "local",
            Scope::Project => "project",
        }
    }
}

/// An MCP server an agent has configured itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Direct {
    /// The agent's id in the agent table (`claude-code`, `codex`).
    pub agent: &'static str,
    /// Its name as people know it.
    pub agent_name: &'static str,
    pub scope: Scope,
    /// The entry's name in the agent's configuration.
    pub name: String,
    /// The URL as configured.
    pub url: String,
    /// The folder a local or project entry belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The file it is in.
    pub file: String,
    /// What takes it out again.
    pub remove: String,
}

/// A URL as two configurations of one server compare: scheme and host in
/// lower case, the scheme's default port dropped, no user, query or
/// fragment, the path without a trailing `/`. `None`: not an http(s) URL.
pub fn normalize(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return None,
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host_port = host_port.to_ascii_lowercase();
    let host_port = match host_port.rsplit_once(':') {
        // Not the colons of an IPv6 address without a port.
        Some((h, p)) if !p.contains(']') && p == default => h.to_string(),
        _ => host_port,
    };
    if host_port.is_empty() {
        return None;
    }
    Some(format!(
        "{scheme}://{host_port}{}",
        path.trim_end_matches('/')
    ))
}

/// Whether two URLs name one server.
pub fn same_url(a: &str, b: &str) -> bool {
    matches!((normalize(a), normalize(b)), (Some(a), Some(b)) if a == b)
}

/// A folder as Claude Code writes it as a key, and as a working tree's
/// path compares to it: `/` for `\`, no trailing `/`, case-insensitive on
/// Windows.
fn folder_key(p: &str) -> String {
    let k = p.replace('\\', "/").trim_end_matches('/').to_string();
    if cfg!(windows) { k.to_lowercase() } else { k }
}

/// `tree` is `folder` or inside it.
fn within(tree: &str, folder: &str) -> bool {
    let (t, f) = (folder_key(tree), folder_key(folder));
    !f.is_empty() && (t == f || t.starts_with(&format!("{f}/")))
}

/// The servers of a Claude Code `mcpServers` object that have a URL.
fn servers_of(v: &Value) -> Vec<(String, String)> {
    v.as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(name, s)| {
                    let url = s.get("url")?.as_str()?;
                    normalize(url)?;
                    Some((name.clone(), url.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn claude_entry(
    scope: Scope,
    name: String,
    url: String,
    folder: Option<String>,
    file: &Path,
) -> Direct {
    let remove = match &folder {
        Some(f) => format!(
            "claude mcp remove --scope {} {name}  (in {f})",
            scope.as_str()
        ),
        None => format!("claude mcp remove --scope {} {name}", scope.as_str()),
    };
    Direct {
        agent: "claude-code",
        agent_name: "Claude Code",
        scope,
        name,
        url,
        folder,
        file: file.display().to_string(),
        remove,
    }
}

/// Claude Code's own servers in `config` (its `.claude.json`): the user's,
/// and the local ones of the folder `tree` is in, or of every folder
/// without a tree.
pub fn claude(config: &Path, tree: Option<&Path>) -> Vec<Direct> {
    let Some(v) = std::fs::read(config)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<Direct> = servers_of(&v["mcpServers"])
        .into_iter()
        .map(|(n, u)| claude_entry(Scope::User, n, u, None, config))
        .collect();
    if let Some(projects) = v["projects"].as_object() {
        let tree = tree.map(|t| t.to_string_lossy().into_owned());
        for (folder, p) in projects {
            if tree.as_deref().is_some_and(|t| !within(t, folder)) {
                continue;
            }
            out.extend(
                servers_of(&p["mcpServers"])
                    .into_iter()
                    .map(|(n, u)| claude_entry(Scope::Local, n, u, Some(folder.clone()), config)),
            );
        }
    }
    out
}

/// The servers of a working tree's `.mcp.json` (Claude Code's project scope).
pub fn claude_project(tree: &Path) -> Vec<Direct> {
    let file = tree.join(".mcp.json");
    let Some(v) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return Vec::new();
    };
    servers_of(&v["mcpServers"])
        .into_iter()
        .map(|(n, u)| {
            claude_entry(
                Scope::Project,
                n,
                u,
                Some(tree.display().to_string()),
                &file,
            )
        })
        .collect()
}

/// Codex's own servers with a URL in `config` (its `config.toml`).
pub fn codex(config: &Path) -> Vec<Direct> {
    let Some(v) = std::fs::read_to_string(config)
        .ok()
        .and_then(|t| toml::from_str::<toml::Value>(&t).ok())
    else {
        return Vec::new();
    };
    let Some(servers) = v.get("mcp_servers").and_then(|s| s.as_table()) else {
        return Vec::new();
    };
    servers
        .iter()
        .filter_map(|(name, s)| {
            let url = s.get("url")?.as_str()?;
            normalize(url)?;
            Some(Direct {
                agent: "codex",
                agent_name: "Codex",
                scope: Scope::User,
                name: name.clone(),
                url: url.to_string(),
                folder: None,
                file: config.display().to_string(),
                remove: format!("delete [mcp_servers.{name}] from {}", config.display()),
            })
        })
        .collect()
}

pub(crate) fn claude_file() -> Option<PathBuf> {
    devstack_skills::agents::resolve(CLAUDE_FILE)
}

pub(crate) fn codex_file() -> Option<PathBuf> {
    devstack_skills::agents::resolve(CODEX_FILE)
}

/// The servers the agent `agent` (its id) has configured itself, as they
/// apply in the working tree `tree`.
pub fn of_agent(agent: &str, tree: Option<&Path>) -> Vec<Direct> {
    match agent {
        "claude-code" => {
            let mut out = claude_file().map_or_else(Vec::new, |f| claude(&f, tree));
            if let Some(t) = tree {
                out.extend(claude_project(t));
            }
            out
        }
        "codex" => codex_file().map_or_else(Vec::new, |f| codex(&f)),
        _ => Vec::new(),
    }
}

/// Every server Claude Code and Codex have configured themselves on this
/// machine, in every folder, and in the `.mcp.json` of the working trees
/// `trees`.
pub fn everywhere(trees: &[PathBuf]) -> Vec<Direct> {
    let mut out = claude_file().map_or_else(Vec::new, |f| claude(&f, None));
    for t in trees {
        out.extend(claude_project(t));
    }
    out.extend(codex_file().map_or_else(Vec::new, |f| codex(&f)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_compare_as_one_server() {
        assert_eq!(
            normalize("HTTPS://Timetrack.Example.COM:443/mcp/?x=1#y").as_deref(),
            Some("https://timetrack.example.com/mcp")
        );
        assert!(same_url(
            "https://user@tracker.example/mcp/",
            "https://tracker.example/mcp"
        ));
        assert!(!same_url(
            "https://tracker.example:8443/mcp",
            "https://tracker.example/mcp"
        ));
        assert!(!same_url(
            "http://tracker.example/mcp",
            "https://tracker.example/mcp"
        ));
        assert!(same_url("http://localhost:80", "http://LOCALHOST/"));
        assert_eq!(normalize("stdio:foo"), None);
        assert_eq!(normalize("ftp://x/y"), None);
        assert_eq!(normalize("https:///mcp"), None);
    }

    #[test]
    fn claude_codes_user_local_and_project_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "mcpServers": {
                    "timetrack": { "type": "http", "url": "https://timetrack.example/mcp", "headers": { "Authorization": "Bearer secret" } },
                    "local-tool": { "command": "npx", "args": ["x"] }
                },
                "projects": {
                    "D:/src/shop": { "mcpServers": { "tracker": { "type": "sse", "url": "https://tracker.example/sse" } } },
                    "D:/src/other": { "mcpServers": { "else": { "url": "https://else.example/" } } }
                }
            })
            .to_string(),
        )
        .unwrap();
        let all = claude(&cfg, None);
        assert_eq!(all.len(), 3, "{all:?}");
        assert!(!format!("{all:?}").contains("secret"), "no header is kept");
        let user = &all[0];
        assert_eq!((user.scope, user.name.as_str()), (Scope::User, "timetrack"));
        assert_eq!(user.remove, "claude mcp remove --scope user timetrack");

        let tree = Path::new("D:/src/shop/apps/web");
        let here = claude(&cfg, Some(tree));
        let names: Vec<&str> = here.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["timetrack", "tracker"]);
        assert_eq!(here[1].scope, Scope::Local);
        assert_eq!(
            here[1].remove,
            "claude mcp remove --scope local tracker  (in D:/src/shop)"
        );

        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{ "mcpServers": { "team": { "type": "http", "url": "https://team.example/mcp" } } }"#,
        )
        .unwrap();
        let p = claude_project(dir.path());
        assert_eq!((p[0].scope, p[0].name.as_str()), (Scope::Project, "team"));
        assert!(
            p[0].remove
                .starts_with("claude mcp remove --scope project team")
        );

        // Not there, or not JSON: nothing.
        assert!(claude(&dir.path().join("none.json"), None).is_empty());
        std::fs::write(&cfg, "{ not json").unwrap();
        assert!(claude(&cfg, None).is_empty());
    }

    #[test]
    fn codexs_servers_with_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(
            &cfg,
            r#"
model = "x"
[mcp_servers.timetrack]
url = "https://timetrack.example/mcp"
bearer_token_env_var = "TT"
[mcp_servers.files]
command = "npx"
"#,
        )
        .unwrap();
        let got = codex(&cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "timetrack");
        assert!(
            got[0]
                .remove
                .starts_with("delete [mcp_servers.timetrack] from ")
        );
    }
}
