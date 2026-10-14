//! HTTP health checks, always against the instance's own port, never through
//! anything in between (ADR-0010 condition 1).

use std::time::Duration;

pub struct Checker {
    agent: ureq::Agent,
    insecure: ureq::Agent,
}

impl Default for Checker {
    fn default() -> Self {
        let base = || {
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(3)))
                .http_status_as_error(false)
        };
        let agent = base().build().new_agent();
        let insecure = base()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build(),
            )
            .build()
            .new_agent();
        Self { agent, insecure }
    }
}

impl Checker {
    /// `Ok(())` on a 2xx answer, otherwise a short reason. Tries IPv4 and IPv6
    /// loopback: a dev server bound to `localhost` may listen on `::1` only
    /// (Node does, on Windows).
    pub fn check(&self, port: u16, path: &str, tls: bool, insecure: bool) -> Result<(), String> {
        let agent = if insecure {
            &self.insecure
        } else {
            &self.agent
        };
        let scheme = if tls { "https" } else { "http" };
        let mut first_error = None;
        for host in ["127.0.0.1", "[::1]"] {
            let url = format!("{scheme}://{host}:{port}{path}");
            let result = match agent.get(&url).call() {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => Err(format!("{url} answered {}", r.status())),
                Err(e) => Err(format!("{url}: {e}")),
            };
            first_error.get_or_insert(result);
        }
        first_error.unwrap_or(Ok(()))
    }
}
