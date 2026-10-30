//! The MCP upstreams a pool names for its members (ADR-0069, option B;
//! review card approved 2026-10-28): what each member's hub may offer its
//! user. The member's hub logs in itself and uses an entry only once its
//! user accepted it.
//!
//! - **URL only** (card point 2): an entry is never a command, so a pool's
//!   admin cannot run anything on a member's machine.
//! - **No credential, no variable**: an entry says how its upstream logs in
//!   (OAuth, a token the member gives its own hub, or none), not with what.
//!   The one exception (review card approved 2026-10-30): the client secret
//!   of an OAuth app registered for desktop clients, for an authorization
//!   server that wants one even with PKCE (GitHub). Such a secret is not
//!   confidential by nature (RFC 8252 §8.5); members get it with the list,
//!   and nothing that shows the list shows it ([`PoolUpstream::redacted`]).
//! - Kept in `upstreams.json` in the coordinator's directory; every change
//!   in the audit log (card point 5).

use std::path::Path;

use serde::{Deserialize, Serialize};

const FILE: &str = "upstreams.json";

/// At most this many entries in a pool's list.
pub const MAX: usize = 64;

/// One MCP upstream as a pool names it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolUpstream {
    /// `[a-z0-9-]`, 1 to 32.
    pub name: String,
    /// Its MCP endpoint (Streamable HTTP), `https://`.
    pub url: String,
    /// What its tools are prefixed with (`<prefix>__<tool>`); the name with
    /// `_` for `-` when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The tools offered; empty: all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default)]
    pub auth: PoolAuth,
}

/// How a pool's upstream logs in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PoolAuth {
    /// No credential.
    #[default]
    None,
    /// A static bearer token each member gives their own hub.
    Token,
    /// The hub's OAuth login. `client_id` (public) where the authorization
    /// server does not register clients itself, `client_secret` with it
    /// where the server wants one even with PKCE.
    Oauth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
    },
}

/// At most this long, a client secret.
const SECRET_MAX: usize = 1024;

impl PoolUpstream {
    pub fn prefix(&self) -> String {
        self.prefix
            .clone()
            .unwrap_or_else(|| self.name.replace('-', "_"))
    }

    /// What is wrong with it, if anything.
    pub fn check(&self) -> Result<(), String> {
        let n = &self.name;
        if n.is_empty()
            || n.len() > 32
            || !n
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!("`{n}`: a name is 1 to 32 of a-z, 0-9 and -"));
        }
        let p = self.prefix();
        if p.is_empty()
            || p.len() > 32
            || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!("`{p}`: a prefix is 1 to 32 of a-z, 0-9 and _"));
        }
        // A pool's `localhost` would be each member's own machine.
        let u = self.url.trim();
        if u != self.url || !u.starts_with("https://") || u.len() <= "https://".len() {
            return Err(format!(
                "`{u}`: a pool's upstream is a URL reached over https"
            ));
        }
        if u.len() > 2048 {
            return Err("the URL is too long".into());
        }
        if self.allow.len() > 256 || self.allow.iter().any(|t| t.is_empty() || t.len() > 128) {
            return Err(
                "the tool allowlist names 1 to 128 characters per tool, 256 at most".into(),
            );
        }
        if let PoolAuth::Oauth {
            client_id,
            client_secret: Some(c),
            ..
        } = &self.auth
            && !c.is_empty()
        {
            if client_id.is_none() {
                return Err("a client secret comes with its client id".into());
            }
            if c.len() > SECRET_MAX || c.chars().any(char::is_whitespace) {
                return Err(format!(
                    "a client secret is up to {SECRET_MAX} characters without spaces"
                ));
            }
        }
        Ok(())
    }

    /// The entry as the dashboard and the CLI show it: a client secret
    /// only as `true` (one is set).
    pub fn redacted(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).expect("an entry serialises");
        if let Some(c) = v["auth"].get_mut("client_secret") {
            *c = serde_json::Value::Bool(true);
        }
        v
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    upstreams: Vec<PoolUpstream>,
}

/// The list in the coordinator's `dir`; none without a file.
pub fn load(dir: &Path) -> Vec<PoolUpstream> {
    std::fs::read(dir.join(FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<File>(&b).ok())
        .map(|f| f.upstreams)
        .unwrap_or_default()
}

fn save(dir: &Path, all: &[PoolUpstream]) -> std::io::Result<()> {
    let path = dir.join(FILE);
    let tmp = path.with_extension(format!("{}.tmp", crate::coordinator::random_hex(4)));
    let bytes = serde_json::to_vec_pretty(&File {
        upstreams: all.to_vec(),
    })
    .expect("the list serialises");
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// Add `u` to the list in `dir`, or replace the entry of its name. A
/// change without a client secret keeps the one set (the dashboard never
/// has it to send back); an empty one takes it away. `Err` with why not
/// (the caller's mistake), or an I/O error as `Ok(Err)`.
pub fn put(dir: &Path, mut u: PoolUpstream) -> Result<std::io::Result<()>, String> {
    let mut all = load(dir);
    if let PoolAuth::Oauth { client_secret, .. } = &mut u.auth {
        match client_secret.as_deref() {
            Some("") => *client_secret = None,
            Some(_) => {}
            None => {
                *client_secret =
                    all.iter()
                        .find(|o| o.name == u.name)
                        .and_then(|o| match &o.auth {
                            PoolAuth::Oauth { client_secret, .. } => client_secret.clone(),
                            _ => None,
                        });
            }
        }
    }
    u.check()?;
    if all
        .iter()
        .any(|o| o.name != u.name && o.prefix() == u.prefix())
    {
        return Err(format!("the prefix `{}` is another upstream's", u.prefix()));
    }
    let full = all.len() >= MAX;
    match all.iter_mut().find(|o| o.name == u.name) {
        Some(o) => *o = u,
        None if full => {
            return Err(format!("a pool names {MAX} upstreams at most"));
        }
        None => all.push(u),
    }
    Ok(save(dir, &all))
}

/// Remove the entry `name` from the list in `dir`: whether there was one.
pub fn remove(dir: &Path, name: &str) -> std::io::Result<bool> {
    let mut all = load(dir);
    let before = all.len();
    all.retain(|u| u.name != name);
    if all.len() == before {
        return Ok(false);
    }
    save(dir, &all).map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(name: &str, url: &str) -> PoolUpstream {
        PoolUpstream {
            name: name.into(),
            url: url.into(),
            auth: PoolAuth::Oauth {
                client_id: None,
                client_secret: None,
                scopes: Vec::new(),
            },
            ..Default::default()
        }
    }

    fn with_secret(u: &mut PoolUpstream, secret: Option<&str>) {
        u.auth = PoolAuth::Oauth {
            client_id: Some("app".into()),
            client_secret: secret.map(str::to_string),
            scopes: Vec::new(),
        };
    }

    fn secret_of(dir: &Path) -> Option<String> {
        match &load(dir)[0].auth {
            PoolAuth::Oauth { client_secret, .. } => client_secret.clone(),
            _ => None,
        }
    }

    #[test]
    fn a_client_secret_is_kept_across_a_change_without_one_and_never_shown() {
        let dir = tempfile::tempdir().unwrap();
        let mut u = up("gh", "https://a.example.com/mcp");
        with_secret(&mut u, Some("s3cret"));
        put(dir.path(), u.clone()).unwrap().unwrap();
        assert_eq!(secret_of(dir.path()).as_deref(), Some("s3cret"));
        // The dashboard sends the entry back without it.
        u.url = "https://b.example.com/mcp".into();
        with_secret(&mut u, None);
        put(dir.path(), u.clone()).unwrap().unwrap();
        assert_eq!(secret_of(dir.path()).as_deref(), Some("s3cret"));
        let shown = load(dir.path())[0].redacted();
        assert_eq!(shown["auth"]["client_secret"], true);
        assert!(!shown.to_string().contains("s3cret"));
        // An empty one takes it away.
        with_secret(&mut u, Some(""));
        put(dir.path(), u.clone()).unwrap().unwrap();
        assert_eq!(secret_of(dir.path()), None);
        assert!(
            load(dir.path())[0].redacted()["auth"]
                .get("client_secret")
                .is_none()
        );
        // Without a client id, none.
        let mut bare = up("x", "https://x.example.com/mcp");
        bare.auth = PoolAuth::Oauth {
            client_id: None,
            client_secret: Some("s".into()),
            scopes: Vec::new(),
        };
        assert!(put(dir.path(), bare).unwrap_err().contains("client id"));
    }

    #[test]
    fn an_entry_is_reached_over_https_and_never_on_the_members_machine() {
        assert!(up("wiki", "https://wiki.example.com/mcp").check().is_ok());
        for bad in [
            "http://wiki.example.com/mcp",
            "http://127.0.0.1:9000/mcp",
            "http://localhost/mcp",
            "https://",
            " https://wiki.example.com/mcp",
        ] {
            assert!(up("wiki", bad).check().is_err(), "{bad}");
        }
        assert!(up("Wiki", "https://wiki.example.com/mcp").check().is_err());
    }

    #[test]
    fn a_command_in_the_list_is_not_read_as_an_entry() {
        // A coordinator's file is the pool's: whatever it says, an entry
        // has no command to carry.
        let v: PoolUpstream = serde_json::from_str(
            r#"{"name":"x","url":"https://x.example.com","command":["calc"],"auth":{"kind":"token","env":"HOME"}}"#,
        )
        .unwrap();
        assert_eq!(v.auth, PoolAuth::Token);
        let back = serde_json::to_value(&v).unwrap();
        assert!(back.get("command").is_none());
        assert!(back["auth"].get("env").is_none());
    }

    #[test]
    fn put_replaces_by_name_refuses_a_taken_prefix_and_remove_says_whether() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), up("wiki", "https://a.example.com/mcp"))
            .unwrap()
            .unwrap();
        put(dir.path(), up("wiki", "https://b.example.com/mcp"))
            .unwrap()
            .unwrap();
        assert_eq!(load(dir.path()).len(), 1);
        assert_eq!(load(dir.path())[0].url, "https://b.example.com/mcp");
        let mut other = up("tracker", "https://c.example.com/mcp");
        other.prefix = Some("wiki".into());
        assert!(put(dir.path(), other).unwrap_err().contains("prefix"));
        assert!(remove(dir.path(), "wiki").unwrap());
        assert!(!remove(dir.path(), "wiki").unwrap());
        assert!(load(dir.path()).is_empty());
    }
}
