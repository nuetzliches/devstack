//! Where devstack's MCP server is registered for the user's coding agents
//! (#492): per repository (the default: the dashboard offers a prompt for
//! a repository's own file), or for the user, in every folder: the hub
//! writes one entry `devstack` into the agent's own configuration, and
//! takes it out again when switched back.
//!
//! - **The user decides**, per agent, on this machine. Neither a pool nor a
//!   stack file does.
//! - **Only its own entry.** The rest of the file stays as it is: its keys in
//!   their order, untouched values as they were written (JSON), its comments
//!   and layout (Codex's TOML). An entry `devstack` the hub did not write is
//!   left alone; one that runs something else than `devstack mcp` is an
//!   error, not overwritten. Switched back, the hub removes only the entry
//!   it wrote (as for the hooks, ADR-0094).
//! - **One name.** The entry is always `devstack`: a repository's entry of
//!   that name is the same server, and an agent loads one of the two. An
//!   entry that runs `devstack mcp` under another name doubles every tool:
//!   [`crate::mcp::doubles`] says so.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The setting's file in the hub's state.
const SETTING: &str = "mcp-scope.json";
/// The entry's name in every agent's configuration.
pub const NAME: &str = "devstack";

/// Where devstack's MCP server is registered for an agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// In each repository's own file (the dashboard's prompt).
    #[default]
    Repository,
    /// In the agent's user configuration: every folder.
    User,
}

/// The setting, per agent id; what the hub wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    #[serde(default)]
    pub agents: BTreeMap<String, Scope>,
    /// The agents whose user entry the hub wrote: only these it removes.
    #[serde(default)]
    pub written: Vec<String>,
}

impl Setting {
    pub fn load(state: &Path) -> Self {
        std::fs::read_to_string(state.join(SETTING))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, state: &Path) -> Result<(), String> {
        std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
        write(
            &state.join(SETTING),
            &serde_json::to_vec_pretty(self).expect("serialises"),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// JSON, the servers under `mcpServers`.
    Json,
    /// Codex's TOML, `[mcp_servers.<name>]`.
    Toml,
}

/// An agent whose user configuration takes an MCP server.
struct Agent {
    id: &'static str,
    name: &'static str,
    format: Format,
    file: fn() -> Option<PathBuf>,
}

fn cursor_file() -> Option<PathBuf> {
    devstack_skills::agents::resolve("~/.cursor/mcp.json")
}

const AGENTS: &[Agent] = &[
    Agent {
        id: "claude-code",
        name: "Claude Code",
        format: Format::Json,
        file: crate::agentmcp::claude_file,
    },
    Agent {
        id: "codex",
        name: "Codex",
        format: Format::Toml,
        file: crate::agentmcp::codex_file,
    },
    Agent {
        id: "cursor",
        name: "Cursor",
        format: Format::Json,
        file: cursor_file,
    },
];

/// An agent as the dashboard shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentView {
    pub id: &'static str,
    pub name: &'static str,
    pub scope: Scope,
    /// Its user configuration.
    pub file: Option<PathBuf>,
    /// Its configuration's folder is there: the agent is installed.
    pub installed: bool,
    /// The user configuration registers `devstack mcp` (by the hub or not).
    pub registered: bool,
    /// Why the entry could not be written or removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What `GET /hub/mcp/scope` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub agents: Vec<AgentView>,
}

/// The agents, their setting and their files as they are now.
pub fn view(state: &Path) -> View {
    view_with(state, &|a| (a.file)(), &BTreeMap::new())
}

fn view_with(
    state: &Path,
    file: &dyn Fn(&Agent) -> Option<PathBuf>,
    errors: &BTreeMap<String, String>,
) -> View {
    let setting = Setting::load(state);
    View {
        agents: AGENTS
            .iter()
            .map(|a| {
                let f = file(a);
                AgentView {
                    id: a.id,
                    name: a.name,
                    scope: setting.agents.get(a.id).copied().unwrap_or_default(),
                    installed: f
                        .as_deref()
                        .and_then(Path::parent)
                        .is_some_and(Path::is_dir),
                    registered: f.as_deref().is_some_and(|f| entry(f, a.format).is_some()),
                    file: f,
                    error: errors.get(a.id).cloned(),
                }
            })
            .collect(),
    }
}

/// Set the scope of the agents in `wanted` (by id): write or remove their
/// user entry, and keep the setting. Unknown ids are refused.
pub fn set(state: &Path, wanted: &BTreeMap<String, Scope>) -> Result<View, String> {
    set_with(state, wanted, &|a| (a.file)())
}

fn set_with(
    state: &Path,
    wanted: &BTreeMap<String, Scope>,
    file: &dyn Fn(&Agent) -> Option<PathBuf>,
) -> Result<View, String> {
    if let Some(id) = wanted.keys().find(|id| !AGENTS.iter().any(|a| a.id == *id)) {
        return Err(format!("no agent `{id}` takes devstack's MCP server here"));
    }
    let mut setting = Setting::load(state);
    let mut errors = BTreeMap::new();
    for a in AGENTS {
        let Some(&scope) = wanted.get(a.id) else {
            continue;
        };
        let Some(path) = file(a) else {
            errors.insert(
                a.id.to_string(),
                "no home folder to find its configuration".into(),
            );
            continue;
        };
        let wrote = setting.written.iter().any(|w| w == a.id);
        let result = match scope {
            Scope::User => put(&path, a.format).map(|written| {
                if written && !wrote {
                    setting.written.push(a.id.to_string());
                }
            }),
            // Only the entry the hub wrote goes.
            Scope::Repository if wrote => remove(&path, a.format).map(|()| {
                setting.written.retain(|w| w != a.id);
            }),
            Scope::Repository => Ok(()),
        };
        match result {
            Ok(()) => {
                setting.agents.insert(a.id.to_string(), scope);
            }
            Err(e) => {
                errors.insert(a.id.to_string(), e);
            }
        }
    }
    setting.agents.retain(|_, s| *s != Scope::Repository);
    setting.save(state)?;
    Ok(view_with(state, file, &errors))
}

/// The command line of the entry.
fn command() -> (&'static str, [&'static str; 1]) {
    ("devstack", ["mcp"])
}

/// The entry `devstack` in `file`, as JSON (`None`: there is none, or the
/// file does not parse).
fn entry(file: &Path, format: Format) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(file).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    match format {
        Format::Json => serde_json::from_str::<serde_json::Value>(text)
            .ok()?
            .get("mcpServers")?
            .get(NAME)
            .cloned(),
        Format::Toml => {
            let t: toml::Table = text.parse().ok()?;
            serde_json::to_value(t.get("mcp_servers")?.get(NAME)?).ok()
        }
    }
}

/// An entry runs `devstack mcp`.
fn runs_devstack(v: &serde_json::Value) -> bool {
    crate::mcp::runs_devstack_mcp(v.get("command"), v.get("args"))
}

/// Write the entry into `file` (made if missing). `Ok(false)`: an entry
/// `devstack` that runs `devstack mcp` was there already (the user's own):
/// left as it is, and not the hub's to remove.
fn put(file: &Path, format: Format) -> Result<bool, String> {
    if let Some(e) = entry(file, format) {
        if runs_devstack(&e) {
            return Ok(false);
        }
        return Err(format!(
            "{}: an entry `{NAME}` that does not run `devstack mcp` is there: not changed",
            file.display()
        ));
    }
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let (cmd, args) = command();
    let out = match format {
        Format::Json => edit_json(file, &text, |servers| {
            let entry = serde_json::json!({ "type": "stdio", "command": cmd, "args": args });
            servers.insert(
                NAME.to_string(),
                RawValue::from_string(serde_json::to_string_pretty(&entry).expect("serialises"))
                    .expect("valid JSON"),
            );
        })?,
        Format::Toml => {
            let mut doc: toml_edit::DocumentMut = text
                .parse()
                .map_err(|e| format!("{}: not TOML ({e}): not changed", file.display()))?;
            let servers = doc
                .entry("mcp_servers")
                .or_insert_with(|| {
                    let mut t = toml_edit::Table::new();
                    t.set_implicit(true);
                    toml_edit::Item::Table(t)
                })
                .as_table_mut()
                .ok_or_else(|| {
                    format!(
                        "{}: `mcp_servers` is not a table: not changed",
                        file.display()
                    )
                })?;
            let mut t = toml_edit::Table::new();
            t.insert("command", toml_edit::value(cmd));
            t.insert(
                "args",
                toml_edit::value(toml_edit::Array::from_iter(args.iter().copied())),
            );
            servers.insert(NAME, toml_edit::Item::Table(t));
            doc.to_string()
        }
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    write(file, out.as_bytes())?;
    Ok(true)
}

/// Take the entry out of `file`, if it still runs `devstack mcp`.
fn remove(file: &Path, format: Format) -> Result<(), String> {
    match entry(file, format) {
        Some(e) if runs_devstack(&e) => {}
        // Gone, or changed by the user since: theirs now.
        _ => return Ok(()),
    }
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let out = match format {
        Format::Json => edit_json(file, &text, |servers| {
            servers.shift_remove(NAME);
        })?,
        Format::Toml => {
            let mut doc: toml_edit::DocumentMut = text
                .parse()
                .map_err(|e| format!("{}: not TOML ({e}): not changed", file.display()))?;
            if let Some(t) = doc
                .get_mut("mcp_servers")
                .and_then(|t| t.as_table_like_mut())
            {
                t.remove(NAME);
                if t.is_empty() {
                    doc.remove("mcp_servers");
                }
            }
            doc.to_string()
        }
    };
    write(file, out.as_bytes())
}

/// `text` (a JSON object, or empty) with its `mcpServers` changed by
/// `change`: the other keys stay in their order and as they were written.
fn edit_json(
    file: &Path,
    text: &str,
    change: impl FnOnce(&mut IndexMap<String, Box<RawValue>>),
) -> Result<String, String> {
    let src = text.trim_start_matches('\u{feff}');
    let mut doc: IndexMap<String, Box<RawValue>> = if src.trim().is_empty() {
        IndexMap::new()
    } else {
        serde_json::from_str(src)
            .map_err(|e| format!("{}: not a JSON object ({e}): not changed", file.display()))?
    };
    let mut servers: IndexMap<String, Box<RawValue>> = match doc.get("mcpServers") {
        Some(s) => serde_json::from_str(s.get()).map_err(|e| {
            format!(
                "{}: `mcpServers` is not an object ({e}): not changed",
                file.display()
            )
        })?,
        None => IndexMap::new(),
    };
    change(&mut servers);
    if servers.is_empty() {
        doc.shift_remove("mcpServers");
    } else {
        doc.insert(
            "mcpServers".into(),
            RawValue::from_string(serde_json::to_string_pretty(&servers).expect("serialises"))
                .expect("valid JSON"),
        );
    }
    let mut out = serde_json::to_string_pretty(&doc).expect("serialises");
    out.push('\n');
    Ok(out)
}

/// Write `bytes` to `path` through a file beside it.
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("devstack-{:08x}.tmp", rand::random::<u32>()));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path) -> impl Fn(&Agent) -> Option<PathBuf> + '_ {
        move |a| {
            Some(match a.id {
                "claude-code" => dir.join(".claude.json"),
                "codex" => dir.join(".codex").join("config.toml"),
                _ => dir.join(".cursor").join("mcp.json"),
            })
        }
    }

    fn only(id: &str, scope: Scope) -> BTreeMap<String, Scope> {
        BTreeMap::from([(id.to_string(), scope)])
    }

    #[test]
    fn the_users_entry_comes_and_goes_and_the_rest_of_the_file_stays() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let claude = home.path().join(".claude.json");
        // Keys in an order serde would not keep, a value as written.
        let before = "{\n  \"zeta\": 1,\n  \"numStartups\": 42,\n  \"mcpServers\": {\n    \"tt\": { \"type\": \"http\", \"url\": \"https://t.example/mcp\" }\n  },\n  \"alpha\": {\"a\":[1,2]}\n}\n";
        std::fs::write(&claude, before).unwrap();
        let v = set_with(
            state.path(),
            &only("claude-code", Scope::User),
            &files(home.path()),
        )
        .unwrap();
        let a = v.agents.iter().find(|a| a.id == "claude-code").unwrap();
        assert_eq!(
            (a.scope, a.registered, a.error.as_deref()),
            (Scope::User, true, None)
        );
        let text = std::fs::read_to_string(&claude).unwrap();
        let keys: Vec<&str> = ["\"zeta\"", "\"numStartups\"", "\"mcpServers\"", "\"alpha\""]
            .into_iter()
            .filter(|k| text.contains(k))
            .collect();
        assert_eq!(keys.len(), 4, "{text}");
        assert!(text.find("\"zeta\"") < text.find("\"alpha\""), "{text}");
        assert!(
            text.contains("{\"a\":[1,2]}"),
            "untouched as written: {text}"
        );
        assert!(text.contains("https://t.example/mcp"), "{text}");
        assert!(crate::mcp::registered_in_home(home.path()));
        // Switched back: the entry goes, the file is as it was.
        set_with(
            state.path(),
            &only("claude-code", Scope::Repository),
            &files(home.path()),
        )
        .unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
        let was: serde_json::Value = serde_json::from_str(before).unwrap();
        assert_eq!(after, was);
        assert_eq!(Setting::load(state.path()), Setting::default());
    }

    #[test]
    fn codex_keeps_its_comments_and_the_users_own_entry_stays() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        let before = "# my settings\nmodel = \"gpt-5\"\n\n[mcp_servers.tt]\nurl = \"https://t.example/mcp\" # tracker\n";
        std::fs::write(&codex, before).unwrap();
        set_with(
            state.path(),
            &only("codex", Scope::User),
            &files(home.path()),
        )
        .unwrap();
        let text = std::fs::read_to_string(&codex).unwrap();
        assert!(
            text.starts_with("# my settings\nmodel = \"gpt-5\""),
            "{text}"
        );
        assert!(text.contains("# tracker"), "{text}");
        assert!(text.contains("[mcp_servers.devstack]"), "{text}");
        let t: toml::Table = text.parse().unwrap();
        assert_eq!(
            t["mcp_servers"]["devstack"]["command"].as_str(),
            Some("devstack")
        );
        set_with(
            state.path(),
            &only("codex", Scope::Repository),
            &files(home.path()),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), before);

        // An entry of the user's own: kept on, and kept when switched back.
        let own =
            "[mcp_servers.devstack]\ncommand = \"C:\\\\tools\\\\devstack.exe\"\nargs = [\"mcp\"]\n";
        std::fs::write(&codex, own).unwrap();
        set_with(
            state.path(),
            &only("codex", Scope::User),
            &files(home.path()),
        )
        .unwrap();
        set_with(
            state.path(),
            &only("codex", Scope::Repository),
            &files(home.path()),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), own);
    }

    #[test]
    fn an_entry_of_that_name_for_something_else_is_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let cursor = home.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(cursor.parent().unwrap()).unwrap();
        let before = r#"{"mcpServers":{"devstack":{"command":"node","args":["other.js"]}}}"#;
        std::fs::write(&cursor, before).unwrap();
        let v = set_with(
            state.path(),
            &only("cursor", Scope::User),
            &files(home.path()),
        )
        .unwrap();
        let a = v.agents.iter().find(|a| a.id == "cursor").unwrap();
        assert!(
            a.error
                .as_deref()
                .is_some_and(|e| e.contains("does not run")),
            "{a:?}"
        );
        assert_eq!(a.scope, Scope::Repository);
        assert_eq!(std::fs::read_to_string(&cursor).unwrap(), before);
        assert!(set_with(state.path(), &only("vim", Scope::User), &files(home.path())).is_err());
    }

    #[test]
    fn a_missing_file_is_made_and_removed_to_nothing_but_braces() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let cursor = home.path().join(".cursor").join("mcp.json");
        set_with(
            state.path(),
            &only("cursor", Scope::User),
            &files(home.path()),
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cursor).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["devstack"]["args"][0], "mcp");
        set_with(
            state.path(),
            &only("cursor", Scope::Repository),
            &files(home.path()),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&cursor).unwrap().trim(), "{}");
    }
}
