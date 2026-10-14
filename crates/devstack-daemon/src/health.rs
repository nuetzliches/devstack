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
    /// `Ok(())` on a 2xx answer, otherwise a short reason.
    pub fn check(&self, port: u16, path: &str, tls: bool, insecure: bool) -> Result<(), String> {
        let url = format!(
            "{}://127.0.0.1:{port}{path}",
            if tls { "https" } else { "http" }
        );
        let agent = if insecure {
            &self.insecure
        } else {
            &self.agent
        };
        match agent.get(&url).call() {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("{url} answered {}", r.status())),
            Err(e) => Err(format!("{url}: {e}")),
        }
    }
}
