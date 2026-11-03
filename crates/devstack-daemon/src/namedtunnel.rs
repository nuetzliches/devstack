//! The user's named tunnel (ADR-0090, step 2): a Cloudflare Tunnel of their
//! own account under a wildcard `*.<domain>`, which the hub runs once per
//! machine. This file holds what daemons and the hub share of it: the
//! domain and the tunnel's id, never its credentials (those are in the
//! hub's keychain), and how a public link's host is named.
//!
//! The hub writes [`Config`] when the tunnel is set up; a daemon reads it
//! when a link opens or a service starts (`{service.<name>.public_url}`).
//! A tunnel the pool gave (ADR-0092) has the member's suffix in its hosts
//! (`{service}-{lane}-{stack}--{member}.{domain}`) and names the pool.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What the hub knows of the tunnel, beside the token (no secret in it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The wildcard's parent: `dev-example.com` for `*.dev-example.com`.
    pub domain: String,
    /// The tunnel's id (`cloudflared tunnel create` printed it).
    pub tunnel_id: String,
    /// The member's part of its hosts, with a tunnel the pool gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// The pool that gave it; none: the user's own (ADR-0091).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
}

impl Config {
    /// The host of a service's link through this tunnel.
    pub fn host(&self, stack: &str, lane: &str, service: &str) -> String {
        host_with(&self.domain, self.suffix.as_deref(), stack, lane, service)
    }
}

pub fn path() -> PathBuf {
    crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .join("named-tunnel.json")
}

/// The tunnel set up on this machine, if one is.
pub fn read() -> Option<Config> {
    serde_json::from_slice(&std::fs::read(path()).ok()?).ok()
}

pub fn write(c: &Config) -> std::io::Result<()> {
    let p = path();
    std::fs::create_dir_all(p.parent().expect("has a parent"))?;
    std::fs::write(p, serde_json::to_vec_pretty(c)?)
}

pub fn remove() {
    let _ = std::fs::remove_file(path());
}

/// The hub's port, as the hub reads it (`DEVSTACK_HUB_PORT`, else 4250).
pub fn hub_port() -> u16 {
    std::env::var("DEVSTACK_HUB_PORT")
        .ok()
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or(4250)
}

/// `GET /hub/public-links` of the hub on this machine: its connector's
/// state and the hosts it routes.
pub fn hub_links() -> Result<serde_json::Value, String> {
    let token = crate::token::load_or_create().map_err(|e| e.to_string())?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{}/hub/public-links", hub_port()))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("the hub answered {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// A domain as the wildcard has it: lowercase labels of letters, digits and
/// hyphens, at least two of them.
pub fn valid_domain(d: &str) -> bool {
    let labels: Vec<&str> = d.split('.').collect();
    labels.len() >= 2
        && d.len() <= 200
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// The host of a service's public link: `{service}-{lane}-{stack}.{domain}`,
/// without the lane for the main tree (empty `lane`, or `main`). One label
/// below the wildcard, as its certificate covers one level only: each part
/// is lowercased, anything but letters and digits becomes a hyphen, and a
/// label over 63 characters is cut and ended with a hash of the whole, so
/// that two long names stay apart.
pub fn host(domain: &str, stack: &str, lane: &str, service: &str) -> String {
    host_with(domain, None, stack, lane, service)
}

/// [`host`], ended by `--{suffix}` (a pool's tunnel, ADR-0092), which the
/// cut leaves whole. The parts never hold two hyphens in a row, so the
/// suffix is told apart.
pub fn host_with(
    domain: &str,
    suffix: Option<&str>,
    stack: &str,
    lane: &str,
    service: &str,
) -> String {
    let lane = if lane == "main" { "" } else { lane };
    let full = [service, lane, stack]
        .iter()
        .map(|p| part(p))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let tail = suffix.map(|s| format!("--{s}")).unwrap_or_default();
    let room = 63 - tail.len().min(40);
    let base = if full.len() <= room {
        full
    } else {
        let hash = blake3::hash(full.as_bytes()).to_hex();
        format!("{}-{}", full[..room - 9].trim_end_matches('-'), &hash[..8])
    };
    format!("{base}{tail}.{domain}")
}

/// `{service.<name>.public_url}` of a tree (`lane` empty for the main
/// one): `https://<host>` for every service a link may go to, with the
/// tunnel set up; nothing without one.
pub fn public_urls(
    stack: &devstack_config::Stack,
    lane: &str,
) -> std::collections::BTreeMap<String, String> {
    let Some(c) = read() else {
        return Default::default();
    };
    stack
        .services
        .iter()
        .filter(|(_, s)| s.port.is_some() && s.tunnel != Some(false))
        .map(|(n, _)| {
            (
                n.clone(),
                format!("https://{}", c.host(&stack.stack.name, lane, n)),
            )
        })
        .collect()
}

fn part(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// `cloudflared`, if not on the `PATH` (a test's fake, an install elsewhere).
const PROGRAM_VARIABLE: &str = "DEVSTACK_CLOUDFLARED";

/// `cloudflared`'s last error line in its output, without its timestamp and level.
pub fn cloudflared_error(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find_map(|l| {
            l.split_once(" ERR ")
                .map(|(_, rest)| rest.trim().to_string())
        })
        .filter(|e| !e.is_empty())
}

pub fn cloudflared() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(PROGRAM_VARIABLE).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "{PROGRAM_VARIABLE} names {}, which is not a file",
                p.display()
            ))
        };
    }
    find_on_path("cloudflared").ok_or_else(|| {
        "cloudflared is not installed, or not on the PATH of devstack's daemon and hub: \
         `winget install Cloudflare.cloudflared` (Windows), `brew install cloudflared` (macOS), \
         https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/ (Linux); \
         then restart the stack (and the hub, `devstack hub stop`) so they see it"
            .into()
    })
}

/// `name` in the directories of `PATH`, as the shell would find it.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

pub fn quoted(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_one_label_below_the_domain() {
        assert_eq!(host("dev.io", "shop", "", "web"), "web-shop.dev.io");
        assert_eq!(host("dev.io", "shop", "main", "web"), "web-shop.dev.io");
        assert_eq!(
            host("dev.io", "Shop App", "feature/Login_x", "web"),
            "web-feature-login-x-shop-app.dev.io"
        );
        let long = host("dev.io", "shop", &"a".repeat(80), "web");
        let label = long.strip_suffix(".dev.io").unwrap();
        assert_eq!(label.len(), 63, "{label}");
        assert_ne!(long, host("dev.io", "shop", &"a".repeat(81), "web"));
    }

    #[test]
    fn a_pools_host_ends_in_the_members_suffix() {
        assert_eq!(
            host_with("dev.io", Some("anna"), "shop", "", "web"),
            "web-shop--anna.dev.io"
        );
        let long = host_with("dev.io", Some("anna"), "shop", &"a".repeat(80), "web");
        let label = long.strip_suffix(".dev.io").unwrap();
        assert!(label.len() <= 63 && label.ends_with("--anna"), "{label}");
    }

    #[test]
    fn a_domain_is_checked() {
        assert!(valid_domain("dev-example.com"));
        assert!(!valid_domain("localhost"));
        assert!(!valid_domain("Dev.com"));
        assert!(!valid_domain("*.dev.com"));
        assert!(!valid_domain("-a.com"));
    }

    #[test]
    fn the_last_error_line_is_the_reason() {
        let out = "2026-11-03T10:00:00Z INF Starting\n\
                   2026-11-03T10:00:01Z ERR failed to request quick Tunnel: 429 Too Many Requests\n";
        assert_eq!(
            cloudflared_error(out).as_deref(),
            Some("failed to request quick Tunnel: 429 Too Many Requests")
        );
        assert_eq!(cloudflared_error("INF Registered tunnel connection"), None);
    }
}
