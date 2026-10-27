//! Whether a coding agent finds devstack's MCP server (`devstack mcp`) for a
//! repository: an entry in the project's agent configuration, or in the
//! user's. Read only: the dashboard offers a prompt where none is found, and
//! the agent writes the entry (`Prompt::Mcp`).

use std::path::{Path, PathBuf};

/// The servers' table in a configuration file.
#[derive(Clone, Copy)]
enum Format {
    /// JSON, the servers under this key.
    Json(&'static str),
    /// Codex's TOML, `[mcp_servers.<name>]`.
    CodexToml,
}

/// The project's files, relative to the repository: Claude Code, VS Code,
/// Cursor, Codex.
const PROJECT: &[(&str, Format)] = &[
    (".mcp.json", Format::Json("mcpServers")),
    (".vscode/mcp.json", Format::Json("servers")),
    (".cursor/mcp.json", Format::Json("mcpServers")),
    (".codex/config.toml", Format::CodexToml),
];

/// The user's files, relative to the home folder.
const USER: &[(&str, Format)] = &[
    (".claude.json", Format::Json("mcpServers")),
    (".cursor/mcp.json", Format::Json("mcpServers")),
    (".codex/config.toml", Format::CodexToml),
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
        .map(|(rel, f)| (repo.join(rel), *f))
        .filter(|(p, f)| in_file(p, *f, None))
        .map(|(p, _)| p)
        .collect();
    if let Some(home) = home {
        found.extend(
            USER.iter()
                .map(|(rel, f)| (home.join(rel), *f))
                .filter(|(p, f)| in_file(p, *f, Some(repo)))
                .map(|(p, _)| p),
        );
    }
    found
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
    match format {
        Format::Json(key) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => {
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
                    .flat_map(|t| t.values())
                    .any(|s| runs_devstack_mcp(s.get("command"), s.get("args")))
            }
            Err(_) => text.contains("devstack"),
        },
        Format::CodexToml => match text.parse::<toml::Table>() {
            Ok(t) => t
                .get("mcp_servers")
                .and_then(|s| s.as_table())
                .is_some_and(|s| {
                    s.values().any(|v| {
                        let json = |k| v.get(k).and_then(|x| serde_json::to_value(x).ok());
                        runs_devstack_mcp(json("command").as_ref(), json("args").as_ref())
                    })
                }),
            Err(_) => text.contains("devstack"),
        },
    }
}

/// `command` is devstack (a path to it, `.exe` or not), `args` start with `mcp`.
fn runs_devstack_mcp(
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
    fn a_file_that_does_not_parse_counts_when_it_names_devstack() {
        let repo = tempfile::tempdir().unwrap();
        let file = repo.path().join(".vscode/mcp.json");
        write(&file, "{ // devstack\n \"servers\": {} }");
        assert_eq!(registered_with(repo.path(), None), [file]);
    }
}
