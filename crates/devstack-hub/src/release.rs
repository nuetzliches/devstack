//! Is there a newer devstack? One request to the GitHub API for the latest
//! release, made by the hub (at start, every 12 hours by the clock and when a
//! dashboard connects to a stale answer; see `watch_release`) and by
//! `devstack update --check` / `devstack doctor`. `DEVSTACK_NO_UPDATE_CHECK=1`
//! turns the hub's check off (a machine without internet, a policy).

use std::time::Duration;

pub const REPO: &str = "nuetzliches/devstack";

/// TLS for requests to GitHub: the system's certificate store, as the
/// installers use it (a TLS-inspecting proxy whose root the system trusts).
pub fn system_tls() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

/// `0.4.0`, `1.2.3-rc.1`: what may go into a download URL.
pub fn valid_version(v: &str) -> bool {
    let mut parts = v.splitn(2, '-');
    let core = parts.next().unwrap_or("");
    let nums: Vec<&str> = core.split('.').collect();
    nums.len() == 3
        && nums
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none_or(|pre| {
            !pre.is_empty()
                && pre
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
}

/// The latest released version (`0.4.0`), or why not.
pub fn latest() -> Result<String, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .tls_config(system_tls())
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
    let tag = v["tag_name"].as_str().ok_or("no tag_name in the answer")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if !valid_version(version) {
        return Err(format!("the latest release has an unexpected tag `{tag}`"));
    }
    Ok(version.to_string())
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

    #[test]
    fn only_plain_versions_go_into_a_url() {
        for ok in ["0.9.8", "10.0.1", "1.2.3-rc.1"] {
            assert!(valid_version(ok), "{ok}");
        }
        for bad in [
            "",
            "0.9",
            "0.9.8/../x",
            "0.9.8?x",
            "v0.9.8",
            "1.2.3-",
            "a.b.c",
        ] {
            assert!(!valid_version(bad), "{bad}");
        }
    }
}
