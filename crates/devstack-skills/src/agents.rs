//! The coding agents devstack knows (ADR-0074): each one's user skill
//! folder, its skill folder in a working tree (ADR-0079), and how it is
//! told installed, as data (`agents.toml`, taken from
//! vercel-labs/skills by `scripts/sync-agents.py`). The hub installs a
//! pool's skill sets into these folders; "Open with" takes its agents from
//! here too, with what `[agent.open]` adds.
//!
//! A path in the table starts with `~` (the user's home) or
//! `${VAR:-default}` (a variable, else its default); `/etc/...` stays as it
//! is.

use serde::Deserialize;
use std::path::{Path, PathBuf};

const TABLE: &str = include_str!("../agents.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// Its user skill folder, as the table writes it.
    pub skills: String,
    /// Its skill folder in a working tree (`.claude/skills`), where the
    /// skills bound to a repository go (ADR-0079); `None`: it has none.
    #[serde(default)]
    pub project: Option<String>,
    /// Paths of which one exists when it is installed.
    pub detect: Vec<String>,
    #[serde(default)]
    pub open: Option<Open>,
}

/// How "Open with" starts an agent: links of a URL scheme it registered.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Open {
    #[serde(default)]
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Link {
    pub scheme: String,
    pub label: String,
}

#[derive(Deserialize)]
struct Table {
    agent: Vec<Agent>,
}

/// Every agent of the table, in its order.
pub fn all() -> &'static [Agent] {
    static ALL: std::sync::OnceLock<Vec<Agent>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        toml::from_str::<Table>(TABLE)
            .expect("agents.toml is valid")
            .agent
    })
}

/// The agent `id`.
pub fn get(id: &str) -> Option<&'static Agent> {
    all().iter().find(|a| a.id == id)
}

/// The user's home: `USERPROFILE` on Windows, else `HOME`.
pub fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()))
        .map(PathBuf::from)
}

/// A path of the table on this machine, with `env` for variables and
/// `home` for `~`; `None` without a home where one is needed.
pub fn resolve_with(
    p: &str,
    home: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let (base, rest): (String, &str) = if let Some(r) = p.strip_prefix("${") {
        let end = r.find('}')?;
        let (var, default) = r[..end].split_once(":-").unwrap_or((&r[..end], ""));
        let rest = &r[end + 1..];
        match env(var).filter(|v| !v.trim().is_empty()) {
            Some(v) => (v.trim().to_string(), rest),
            None => {
                let d = resolve_with(default, home, env)?;
                (d.to_string_lossy().into_owned(), rest)
            }
        }
    } else if let Some(r) = p.strip_prefix('~') {
        (home?.to_string_lossy().into_owned(), r)
    } else {
        (String::new(), p)
    };
    let mut out = PathBuf::from(base);
    for part in rest.split('/').filter(|s| !s.is_empty()) {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    if p.starts_with('/') && !out.has_root() {
        return Some(PathBuf::from(p));
    }
    Some(out)
}

/// A path of the table on this machine.
pub fn resolve(p: &str) -> Option<PathBuf> {
    resolve_with(p, home().as_deref(), &|v| std::env::var(v).ok())
}

impl Agent {
    /// Its user skill folder on this machine.
    pub fn skills_dir(&self) -> Option<PathBuf> {
        resolve(&self.skills)
    }

    /// Whether it is installed here: one of its `detect` paths exists.
    pub fn installed(&self) -> bool {
        self.detect
            .iter()
            .filter_map(|d| resolve(d))
            .any(|d| d.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_reads_and_knows_the_agents_devstack_users_run() {
        for id in [
            "claude-code",
            "codex",
            "opencode",
            "cursor",
            "github-copilot",
        ] {
            assert!(get(id).is_some(), "{id}");
        }
        assert_eq!(
            get("claude-code").unwrap().project.as_deref(),
            Some(".claude/skills")
        );
        assert_eq!(
            get("codex").unwrap().project.as_deref(),
            Some(".agents/skills")
        );
        let links = &get("claude-code").unwrap().open.as_ref().unwrap().links;
        assert!(links.iter().any(|l| l.scheme == "claude-cli"));
    }

    #[test]
    fn a_path_takes_its_variable_else_the_default_under_home() {
        let home = Path::new("/home/ann");
        let none = |_: &str| None;
        let set = |v: &str| (v == "CLAUDE_CONFIG_DIR").then(|| "/cfg/claude".to_string());
        let p = "${CLAUDE_CONFIG_DIR:-~/.claude}/skills";
        assert_eq!(
            resolve_with(p, Some(home), &none),
            Some(home.join(".claude").join("skills"))
        );
        assert_eq!(
            resolve_with(p, Some(home), &set),
            Some(PathBuf::from("/cfg/claude").join("skills"))
        );
        assert_eq!(
            resolve_with("~/.agents/skills", Some(home), &none),
            Some(home.join(".agents").join("skills"))
        );
        assert_eq!(resolve_with("~/.agents/skills", None, &none), None);
        assert_eq!(
            resolve_with("/etc/codex", None, &none),
            Some(PathBuf::from("/etc/codex"))
        );
    }
}
