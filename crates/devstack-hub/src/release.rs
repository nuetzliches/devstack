//! Is there a newer devstack? One request to the GitHub API for the latest
//! release, made by the hub (at start, every 12 hours by the clock and when a
//! dashboard connects to a stale answer; see `watch_release`) and by
//! `devstack update --check` / `devstack doctor`. `DEVSTACK_NO_UPDATE_CHECK=1`
//! turns the hub's check off (a machine without internet, a policy).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const REPO: &str = "nuetzliches/devstack";

/// Why a check found no answer, and when GitHub takes the next one if it
/// said so (its rate limit).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckError {
    pub message: String,
    pub retry_at: Option<SystemTime>,
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for CheckError {
    fn from(message: String) -> Self {
        CheckError {
            message,
            retry_at: None,
        }
    }
}

impl From<&str> for CheckError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

/// A refusal (`status` not 200) in words. GitHub answers 60 requests an hour
/// without sign-in, counted per IP address: every program behind the same
/// address draws on them, so a 403 with none remaining is that limit, not a
/// private repository. `reset` is its `x-ratelimit-reset` (seconds since the
/// epoch), `retry_after` its `retry-after` (seconds, a secondary limit).
fn refused(
    status: u16,
    remaining: Option<&str>,
    reset: Option<&str>,
    retry_after: Option<&str>,
    now: SystemTime,
    tz: &jiff::tz::TimeZone,
) -> CheckError {
    let num = |h: Option<&str>| h.and_then(|v| v.trim().parse::<u64>().ok());
    let limited =
        matches!(status, 403 | 429) && (num(remaining) == Some(0) || num(retry_after).is_some());
    if !limited {
        return format!("GitHub answered {status}").into();
    }
    let retry_at = num(retry_after)
        .map(|s| now + Duration::from_secs(s))
        .or_else(|| num(reset).map(|s| UNIX_EPOCH + Duration::from_secs(s)));
    let when = retry_at
        .and_then(|t| jiff::Timestamp::try_from(t).ok())
        .map(|t| format!(" until {}", t.to_zoned(tz.clone()).strftime("%H:%M")))
        .unwrap_or_default();
    CheckError {
        message: format!(
            "GitHub's limit for requests without sign-in is used up for this network{when} \
             (60 an hour, shared by every program behind the same address)"
        ),
        retry_at,
    }
}

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
pub fn latest() -> Result<String, CheckError> {
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
        let h = |n: &str| r.headers().get(n).and_then(|v| v.to_str().ok());
        return Err(refused(
            r.status().as_u16(),
            h("x-ratelimit-remaining"),
            h("x-ratelimit-reset"),
            h("retry-after"),
            SystemTime::now(),
            &jiff::tz::TimeZone::system(),
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&r.body_mut().read_to_string().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("no tag_name in the answer")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if !valid_version(version) {
        return Err(format!("the latest release has an unexpected tag `{tag}`").into());
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
    fn the_rate_limit_says_so_and_when_it_ends() {
        let now = UNIX_EPOCH + Duration::from_secs(1_794_315_000);
        let utc = jiff::tz::TimeZone::UTC;
        let e = refused(403, Some("0"), Some("1794315771"), None, now, &utc);
        assert_eq!(
            e.retry_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_794_315_771))
        );
        assert!(e.message.contains("without sign-in"), "{}", e.message);
        assert!(e.message.contains("until 13:02"), "{}", e.message);

        // A secondary limit says how long to wait.
        let e = refused(429, None, None, Some("90"), now, &utc);
        assert_eq!(e.retry_at, Some(now + Duration::from_secs(90)));

        // Any other refusal stays as it was.
        for (status, remaining) in [(403, Some("12")), (404, Some("0")), (500, None)] {
            let e = refused(status, remaining, Some("1794315771"), None, now, &utc);
            assert_eq!(e.message, format!("GitHub answered {status}"));
            assert_eq!(e.retry_at, None);
        }
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
