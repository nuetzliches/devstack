//! Is there a newer devstack? One request to the GitHub API for the latest
//! release, made by the hub (at start, every 12 hours by the clock and when a
//! dashboard connects to a stale answer; see `watch_release`) and by
//! `devstack update --check` / `devstack doctor`. `DEVSTACK_NO_UPDATE_CHECK=1`
//! turns the hub's check off (a machine without internet, a policy).

use std::time::Duration;

pub const REPO: &str = "nuetzliches/devstack";

/// The latest released version (`0.4.0`), or why not.
pub fn latest() -> Result<String, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut r = agent
        .get(&format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", &format!("devstack/{}", crate::VERSION))
        .call()
        .map_err(|e| e.to_string())?;
    if r.status() != 200 {
        return Err(format!("GitHub answered {}", r.status()));
    }
    let v: serde_json::Value =
        serde_json::from_str(&r.body_mut().read_to_string().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    v["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or_else(|| "no tag_name in the answer".into())
}

/// `a` newer than `b` (plain `x.y.z`; anything unparsable is not newer).
pub fn newer(a: &str, b: &str) -> bool {
    let p = |v: &str| -> Option<(u64, u64, u64)> {
        let mut it = v.split(['.', '-']).map(|n| n.parse::<u64>().ok());
        Some((it.next()??, it.next()??, it.next()??))
    };
    matches!((p(a), p(b)), (Some(x), Some(y)) if x > y)
}

pub fn check_disabled() -> bool {
    std::env::var_os("DEVSTACK_NO_UPDATE_CHECK").is_some_and(|v| v != "0" && !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
        assert!(!newer("garbage", "0.3.0"));
    }
}
