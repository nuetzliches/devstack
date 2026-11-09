//! Whether a coding agent finds devstack's MCP server (`devstack mcp`) for a
//! repository: an entry in the project's agent configuration, or in the
//! user's. Read only: the dashboard offers a prompt where none is found, and
//! the agent writes the entry (`Prompt::Mcp`); the user entry for every
//! folder is the hub's to write once the user chose so (`crate::mcpscope`).
//! Two entries that run `devstack mcp` under different names load it twice
//! ([`doubles`]).

use std::path::{Path, PathBuf};

/// The servers' table in a configuration file.
#[derive(Clone, Copy)]
enum Format {
    /// JSON, the servers under this key.
    Json(&'static str),
    /// Codex's TOML, `[mcp_servers.<name>]`.
    CodexToml,
}

/// The project's files, relative to the repository, and the agent that
/// reads each: Claude Code, VS Code, Cursor, Codex.
const PROJECT: &[(&str, Format, &str)] = &[
    (".mcp.json", Format::Json("mcpServers"), "Claude Code"),
    (".vscode/mcp.json", Format::Json("servers"), "VS Code"),
    (".cursor/mcp.json", Format::Json("mcpServers"), "Cursor"),
    (".codex/config.toml", Format::CodexToml, "Codex"),
];

/// The user's files, relative to the home folder.
const USER: &[(&str, Format, &str)] = &[
    (".claude.json", Format::Json("mcpServers"), "Claude Code"),
    (".cursor/mcp.json", Format::Json("mcpServers"), "Cursor"),
    (".codex/config.toml", Format::CodexToml, "Codex"),
];

/// The files that register `devstack mcp` for `repo`: the project's first,
/// then the user's (Claude Code's per-project entries in `~/.claude.json`
/// included).
pub fn registered(repo: &Path) -> Vec<PathBuf> {
    registered_with(repo, home().as_deref())
}

fn registered_with(repo: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = PROJECT
        .iter()
        .map(|(rel, f, _)| (repo.join(rel), *f))
        .filter(|(p, f)| in_file(p, *f, None))
        .map(|(p, _)| p)
        .collect();
    if let Some(home) = home {
        found.extend(
            USER.iter()
                .map(|(rel, f, _)| (home.join(rel), *f))
                .filter(|(p, f)| in_file(p, *f, Some(repo)))
                .map(|(p, _)| p),
        );
    }
    found
}

/// The user's own files that register `devstack mcp` for every folder
/// (their top-level servers, not one project's).
#[cfg(test)]
fn for_every_folder_in(home: &Path) -> Vec<PathBuf> {
    USER.iter()
        .map(|(rel, f, _)| (home.join(rel), *f))
        .filter(|(p, f)| !names_in(p, *f, None).is_empty())
        .map(|(p, _)| p)
        .collect()
}

#[cfg(test)]
pub(crate) fn registered_in_home(home: &Path) -> bool {
    !for_every_folder_in(home).is_empty()
}

/// An entry that runs `devstack mcp` beside another one for the same agent
/// under a different name: the agent loads the server twice, every tool
/// doubled.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Double {
    pub agent: &'static str,
    pub file: PathBuf,
    pub name: String,
}

/// The entries that double `devstack mcp` for an agent in `repo`: per agent,
/// when its files name it under more than one name, every one not named
/// `devstack` (the name the hub writes).
pub fn doubles(repo: &Path) -> Vec<Double> {
    doubles_with(repo, home().as_deref())
}

fn doubles_with(repo: &Path, home: Option<&Path>) -> Vec<Double> {
    let mut all: Vec<Double> = Vec::new();
    let mut add = |agent: &'static str, file: PathBuf, format: Format, repo: Option<&Path>| {
        for name in names_in(&file, format, repo) {
            all.push(Double {
                agent,
                file: file.clone(),
                name,
            });
        }
    };
    for (rel, f, agent) in PROJECT {
        add(agent, repo.join(rel), *f, None);
    }
    if let Some(home) = home {
        for (rel, f, agent) in USER {
            add(agent, home.join(rel), *f, Some(repo));
        }
    }
    let mut out = Vec::new();
    for d in &all {
        let names: std::collections::BTreeSet<&str> = all
            .iter()
            .filter(|x| x.agent == d.agent)
            .map(|x| x.name.as_str())
            .collect();
        if names.len() > 1 && d.name != crate::mcpscope::NAME && !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `file` names a server that runs `devstack mcp`. `repo`: `~/.claude.json`'s
/// entries for that project count too. A file that does not parse (VS Code
/// takes comments in JSON) counts when it names devstack at all: a guess
/// that spares a nag rather than makes one.
fn in_file(file: &Path, format: Format, repo: Option<&Path>) -> bool {
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    let parses = match format {
        Format::Json(_) => serde_json::from_str::<serde_json::Value>(&text).is_ok(),
        Format::CodexToml => text.parse::<toml::Table>().is_ok(),
    };
    if !parses {
        return text.contains("devstack");
    }
    !names_in(file, format, repo).is_empty()
}

/// The names under which `file` runs `devstack mcp`: its servers, and with
/// `repo` Claude Code's entries for that project. A file that does not
/// parse names none.
fn names_in(file: &Path, format: Format, repo: Option<&Path>) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    match format {
        Format::Json(key) => {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                return Vec::new();
            };
            let mut tables = vec![v.get(key)];
            if let (Some(repo), Some(projects)) =
                (repo, v.get("projects").and_then(|p| p.as_object()))
            {
                tables.extend(
                    projects
                        .iter()
                        .filter(|(k, _)| same_path(k, repo))
                        .map(|(_, p)| p.get(key)),
                );
            }
            tables
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_object())
                .flat_map(|t| t.iter())
                .filter(|(_, s)| runs_devstack_mcp(s.get("command"), s.get("args")))
                .map(|(n, _)| n.clone())
                .collect()
        }
        Format::CodexToml => {
            let Ok(t) = text.parse::<toml::Table>() else {
                return Vec::new();
            };
            t.get("mcp_servers")
                .and_then(|s| s.as_table())
                .into_iter()
                .flat_map(|s| s.iter())
                .filter(|(_, v)| {
                    let json = |k| v.get(k).and_then(|x| serde_json::to_value(x).ok());
                    runs_devstack_mcp(json("command").as_ref(), json("args").as_ref())
                })
                .map(|(n, _)| n.clone())
                .collect()
        }
    }
}

/// `command` is devstack (a path to it, `.exe` or not), `args` start with `mcp`.
pub(crate) fn runs_devstack_mcp(
    command: Option<&serde_json::Value>,
    args: Option<&serde_json::Value>,
) -> bool {
    // Either separator: a file may name a Windows path on any machine.
    let devstack = command
        .and_then(|c| c.as_str())
        .and_then(|c| c.rsplit(['/', '\\']).next())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("devstack") || name.eq_ignore_ascii_case("devstack.exe")
        });
    let mcp = args
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.as_str())
        == Some("mcp");
    devstack && mcp
}

/// `key` (a path as an agent wrote it) is `repo`: separators and, on
/// Windows, case aside.
fn same_path(key: &str, repo: &Path) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(key) == norm(&repo.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn finds_the_entry_in_the_projects_files() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let r = repo.path();
        assert!(registered_with(r, Some(home.path())).is_empty());
        write(
            &r.join(".mcp.json"),
            r#"{ "mcpServers": { "playwright": { "command": "npx", "args": ["-y", "x"] } } }"#,
        );
        assert!(registered_with(r, None).is_empty(), "another server");
        write(
            &r.join(".mcp.json"),
            r#"{ "mcpServers": { "ds": { "command": "C:\\tools\\devstack.exe", "args": ["mcp"] } } }"#,
        );
        write(
            &r.join(".vscode/mcp.json"),
            r#"{ "servers": { "devstack": { "type": "stdio", "command": "devstack", "args": ["mcp"] } } }"#,
        );
        write(
            &r.join(".codex/config.toml"),
            "[mcp_servers.devstack]\ncommand = \"devstack\"\nargs = [\"mcp\"]\n",
        );
        assert_eq!(registered_with(r, None).len(), 3);
    }

    #[test]
    fn finds_the_users_entry_and_claude_codes_for_this_project() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let claude = home.path().join(".claude.json");
        let entry = r#"{ "command": "devstack", "args": ["mcp"] }"#;
        let other = Path::new("/elsewhere").to_string_lossy().to_string();
        write(
            &claude,
            &format!(
                r#"{{ "projects": {{ {} : {{ "mcpServers": {{ "devstack": {entry} }} }} }} }}"#,
                serde_json::to_string(&other).unwrap()
            ),
        );
        assert!(
            registered_with(repo.path(), Some(home.path())).is_empty(),
            "another project's"
        );
        let this = repo.path().to_string_lossy().replace('\\', "/");
        write(
            &claude,
            &format!(
                r#"{{ "projects": {{ {} : {{ "mcpServers": {{ "devstack": {entry} }} }} }} }}"#,
                serde_json::to_string(&this).unwrap()
            ),
        );
        assert_eq!(
            registered_with(repo.path(), Some(home.path())),
            std::slice::from_ref(&claude)
        );
        write(
            &claude,
            &format!(r#"{{ "mcpServers": {{ "devstack": {entry} }} }}"#),
        );
        assert_eq!(registered_with(repo.path(), Some(home.path())), [claude]);
    }

    #[test]
    fn two_names_for_one_agent_are_a_double() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let entry = r#"{ "command": "devstack", "args": ["mcp"] }"#;
        write(
            &home.path().join(".claude.json"),
            &format!(r#"{{ "mcpServers": {{ "devstack": {entry} }} }}"#),
        );
        assert!(doubles_with(repo.path(), Some(home.path())).is_empty());
        // The same name in the project: one server.
        write(
            &repo.path().join(".mcp.json"),
            &format!(r#"{{ "mcpServers": {{ "devstack": {entry} }} }}"#),
        );
        assert!(doubles_with(repo.path(), Some(home.path())).is_empty());
        // Another name: loaded twice. Another agent's file is no double.
        write(
            &repo.path().join(".mcp.json"),
            &format!(r#"{{ "mcpServers": {{ "ds": {entry} }} }}"#),
        );
        write(
            &repo.path().join(".vscode/mcp.json"),
            &format!(r#"{{ "servers": {{ "ds2": {entry} }} }}"#),
        );
        let d = doubles_with(repo.path(), Some(home.path()));
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].agent, d[0].name.as_str()), ("Claude Code", "ds"));
        assert_eq!(for_every_folder_in(home.path()).len(), 1);
    }

    #[test]
    fn a_file_that_does_not_parse_counts_when_it_names_devstack() {
        let repo = tempfile::tempdir().unwrap();
        let file = repo.path().join(".vscode/mcp.json");
        write(&file, "{ // devstack\n \"servers\": {} }");
        assert_eq!(registered_with(repo.path(), None), [file]);
    }
}
