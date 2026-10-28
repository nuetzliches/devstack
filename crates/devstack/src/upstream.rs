//! The tools of the user's MCP upstreams in `devstack mcp` (ADR-0069; review
//! card on #316, points 2, 5, 6): listed beside devstack's own as
//! `<prefix>__<tool>`, and called through the hub, which holds the logins.
//! The hub is started when it does not run. Without a hub
//! (`DEVSTACK_NO_HUB`), only upstreams without a login or with a token from
//! the environment are asked, from here.

use serde_json::{Value, json};
use std::time::Duration;

/// The hub's answer, or `None` when there is none to ask.
fn hub(path: &str, body: Option<Value>, within: Duration) -> Option<Value> {
    if std::env::var_os("DEVSTACK_NO_HUB").is_some() {
        return None;
    }
    let port = devstack_hub::ensure_running().ok()?;
    let token = devstack_daemon::token::load().filter(|t| !t.is_empty())?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(within))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let url = format!("http://127.0.0.1:{port}{path}");
    let auth = format!("Bearer {token}");
    let mut r = match body {
        Some(b) => agent
            .post(&url)
            .header("Authorization", &auth)
            .header("X-Devstack-Client", "mcp")
            .header("Content-Type", "application/json")
            .send(b.to_string())
            .ok()?,
        None => agent
            .get(&url)
            .header("Authorization", &auth)
            .header("X-Devstack-Client", "mcp")
            .call()
            .ok()?,
    };
    if r.status() != 200 {
        return None;
    }
    serde_json::from_str(&r.body_mut().read_to_string().ok()?).ok()
}

/// The user configured upstreams at all, or accepted one of a pool: else
/// neither the hub is started nor anything asked.
fn configured() -> bool {
    devstack_hub::upstreams::Upstreams::configured(&devstack_hub::upstreams::dir())
}

fn direct() -> devstack_hub::upstreams::Upstreams {
    devstack_hub::upstreams::Upstreams::without_secrets(devstack_hub::upstreams::dir())
}

/// The upstreams' tools as an agent sees them.
pub fn tools() -> Vec<Value> {
    if !configured() {
        return Vec::new();
    }
    if std::env::var_os("DEVSTACK_NO_HUB").is_some() {
        return direct().tools(&devstack_hub::upstreams::http());
    }
    hub("/hub/upstreams/tools", None, Duration::from_secs(60))
        .and_then(|v| v["tools"].as_array().cloned())
        .unwrap_or_default()
}

/// Call the upstream tool `name`: its `CallToolResult`.
pub fn call(name: &str, arguments: Value) -> Value {
    if std::env::var_os("DEVSTACK_NO_HUB").is_some() {
        return direct().call(&devstack_hub::upstreams::calls(), name, arguments);
    }
    hub(
        "/hub/upstreams/call",
        Some(json!({ "name": name, "arguments": arguments })),
        Duration::from_secs(600),
    )
    .unwrap_or_else(|| {
        json!({
            "content": [{ "type": "text", "text": format!(
                "`{name}` goes through the devstack hub, which does not answer: `devstack hub start`, then try again"
            ) }],
            "isError": true,
        })
    })
}

/// Whether `name` is an upstream's tool rather than devstack's own: its own
/// have no `__`.
pub fn is_upstream(name: &str) -> bool {
    name.contains("__")
}

#[cfg(test)]
mod tests {
    #[test]
    fn devstacks_own_tools_never_look_like_an_upstreams() {
        for t in crate::mcp::tools() {
            let name = t["name"].as_str().unwrap();
            assert!(!super::is_upstream(name), "{name}");
        }
        assert!(super::is_upstream("tracker__search"));
    }
}
