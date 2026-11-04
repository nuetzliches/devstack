//! The hooks a skill carries (ADR-0094): `hooks.toml` beside its
//! `SKILL.md`, read when the coordinator fetches a set (a wrong file leaves
//! the skill out) and again by each hub from the archive it installs.
//!
//! ```toml
//! [[hook]]
//! event = "prompt-submit"          # or "session-start"
//! match = '(^|\W)ABC-[0-9]+\b'     # a regex on the prompt; prompt-submit only
//! once = "match"                   # "session" or "match"
//! upstream = "tracker"             # only while the pool's upstream is accepted
//! context_file = "hooks/ticket.md" # or `context = "..."`
//! ```
//!
//! A hook only adds text to the agent's context: no command of a pool runs
//! on a member's machine.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read as _;

/// The file in a skill's folder.
pub const FILE: &str = "hooks.toml";
/// At most this many hooks a skill.
pub const MAX_HOOKS: usize = 8;
/// At most this many characters a hook's text.
pub const MAX_CONTEXT: usize = 2000;
/// At most this many characters a pattern.
const MAX_PATTERN: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// A session starts (and starts again after its context was cleared or
    /// compacted).
    SessionStart,
    /// The user sent a prompt, before the model reads it.
    PromptSubmit,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Event::SessionStart => "session-start",
            Event::PromptSubmit => "prompt-submit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session-start" => Some(Event::SessionStart),
            "prompt-submit" => Some(Event::PromptSubmit),
            _ => None,
        }
    }
}

/// How often a hook's text is given in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Once {
    /// Once a session.
    Session,
    /// Once a session for each text its pattern matches.
    Match,
}

/// A hook as `hooks.toml` writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    event: String,
    #[serde(default, rename = "match")]
    pattern: Option<String>,
    #[serde(default)]
    once: Option<Once>,
    #[serde(default)]
    upstream: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    context_file: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    hook: Vec<Raw>,
}

/// A hook, checked, its text read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hook {
    pub event: Event,
    /// A regex on the prompt (`prompt-submit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub once: Once,
    /// The pool's MCP upstream it belongs to: given only while the member
    /// accepted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub context: String,
}

/// The hooks of a skill whose files are `files` (path → content); none
/// without a `hooks.toml`; why not, for one that is wrong.
pub fn of_files(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<Hook>, String> {
    let Some(text) = files.get(FILE) else {
        return Ok(Vec::new());
    };
    let text = std::str::from_utf8(text).map_err(|_| format!("{FILE}: not UTF-8"))?;
    let raw: RawFile = toml::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
    if raw.hook.len() > MAX_HOOKS {
        return Err(format!("{FILE}: {MAX_HOOKS} hooks at most"));
    }
    raw.hook
        .into_iter()
        .enumerate()
        .map(|(i, r)| check(r, files).map_err(|e| format!("{FILE}, hook {}: {e}", i + 1)))
        .collect()
}

fn check(r: Raw, files: &BTreeMap<String, Vec<u8>>) -> Result<Hook, String> {
    let event = Event::parse(&r.event)
        .ok_or_else(|| format!("`{}`: the event is session-start or prompt-submit", r.event))?;
    if let Some(p) = &r.pattern {
        if event != Event::PromptSubmit {
            return Err("`match` is for prompt-submit only".into());
        }
        if p.is_empty() || p.chars().count() > MAX_PATTERN {
            return Err(format!("`match` is 1 to {MAX_PATTERN} characters"));
        }
        regex::Regex::new(p).map_err(|e| format!("`match`: {e}"))?;
    }
    let once = match (r.once, &r.pattern) {
        (Some(Once::Match), None) => return Err("`once = \"match\"` needs a `match`".into()),
        (Some(o), _) => o,
        (None, Some(_)) => Once::Match,
        (None, None) => Once::Session,
    };
    if let Some(u) = &r.upstream
        && (u.is_empty()
            || u.len() > 32
            || !u
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
    {
        return Err(format!(
            "`{u}`: an upstream's name is 1 to 32 of a-z, 0-9 and -"
        ));
    }
    let context = match (r.context, r.context_file) {
        (Some(c), None) => c,
        (None, Some(f)) => {
            let bytes = files
                .get(&f)
                .ok_or_else(|| format!("`{f}`: no such file in the skill"))?;
            String::from_utf8(bytes.clone()).map_err(|_| format!("`{f}`: not UTF-8"))?
        }
        _ => return Err("one of `context` and `context_file`".into()),
    };
    let context = context.trim().to_string();
    if context.is_empty() || context.chars().count() > MAX_CONTEXT {
        return Err(format!("its text is 1 to {MAX_CONTEXT} characters"));
    }
    Ok(Hook {
        event,
        pattern: r.pattern,
        once,
        upstream: r.upstream,
        context,
    })
}

/// The hooks of a skill's archive (`.tar.gz`, as a set's fetch packs it).
pub fn of_archive(bytes: &[u8]) -> Result<Vec<Hook>, String> {
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = e
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        total += e.header().size().unwrap_or(0);
        if total > crate::skills::MAX_UNPACKED {
            return Err("over the limits of a skill".into());
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        files.insert(path, buf);
    }
    of_files(&files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(hooks: &str) -> BTreeMap<String, Vec<u8>> {
        let mut f = BTreeMap::new();
        f.insert(FILE.to_string(), hooks.as_bytes().to_vec());
        f.insert(
            "hooks/ticket.md".to_string(),
            b"Ticket checklist\n".to_vec(),
        );
        f
    }

    #[test]
    fn a_hooks_file_gives_its_hooks_with_their_texts() {
        let h = of_files(&files(
            r#"
[[hook]]
event = "session-start"
upstream = "nuts-timetrack"
context = "  Load the skill now.  "

[[hook]]
event = "prompt-submit"
match = '(^|\W)ABC-[0-9]+\b'
context_file = "hooks/ticket.md"
"#,
        ))
        .unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].event, Event::SessionStart);
        assert_eq!(h[0].once, Once::Session);
        assert_eq!(h[0].context, "Load the skill now.");
        assert_eq!(h[0].upstream.as_deref(), Some("nuts-timetrack"));
        assert_eq!(h[1].once, Once::Match);
        assert_eq!(h[1].context, "Ticket checklist");
        assert!(of_files(&BTreeMap::new()).unwrap().is_empty());
    }

    #[test]
    fn a_wrong_hook_is_refused_with_why() {
        let wrong = |s: &str| of_files(&files(s)).unwrap_err();
        assert!(wrong("[[hook]]\nevent = \"tool-use\"\ncontext = \"x\"").contains("event"));
        assert!(
            wrong("[[hook]]\nevent = \"session-start\"\nmatch = \"x\"\ncontext = \"x\"")
                .contains("prompt-submit only")
        );
        assert!(
            wrong("[[hook]]\nevent = \"prompt-submit\"\nmatch = \"(\"\ncontext = \"x\"")
                .contains("`match`")
        );
        assert!(
            wrong("[[hook]]\nevent = \"prompt-submit\"\nonce = \"match\"\ncontext = \"x\"")
                .contains("needs a `match`")
        );
        assert!(wrong("[[hook]]\nevent = \"session-start\"").contains("one of"));
        assert!(
            wrong("[[hook]]\nevent = \"session-start\"\ncontext_file = \"../x.md\"")
                .contains("no such file")
        );
        assert!(
            wrong("[[hook]]\nevent = \"session-start\"\ncommand = \"rm -rf /\"\ncontext = \"x\"")
                .contains("command")
        );
        let long = format!(
            "[[hook]]\nevent = \"session-start\"\ncontext = \"{}\"",
            "x".repeat(MAX_CONTEXT + 1)
        );
        assert!(wrong(&long).contains("characters"));
    }
}
