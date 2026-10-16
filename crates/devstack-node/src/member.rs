//! A machine's membership in a pool (ADR-0031 point 2): its key, its request
//! to join, and once approved its certificate, in its own directory. Every
//! call to the coordinator after the first is mutual TLS with the pool's CA.

use crate::coordinator::{JoinAnswer, JoinRequest, MemberView, NodeView, Report};
use crate::pki;
use crate::store::State;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum MemberError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}")]
    Pki(#[from] pki::PkiError),
    #[error("coordinator: {0}")]
    Http(#[from] ureq::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MemberError>;

/// `member.json`: which pool this machine belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    /// The coordinator's base URL, `https://host:port`.
    pub coordinator: String,
    /// SHA-256 of the pool's CA certificate.
    pub ca_fingerprint: String,
    pub id: String,
    pub name: String,
    pub csr: String,
    pub state: State,
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> MemberError + '_ {
    move |e| MemberError::Io(path.into(), e)
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(io(path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io(path))?;
    }
    Ok(())
}

/// Lower case, without the colons and spaces a fingerprint is often written
/// with.
pub fn normalise_fingerprint(fp: &str) -> String {
    fp.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn agent(tls: ureq::tls::TlsConfig) -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn tls_builder() -> ureq::tls::TlsConfigBuilder {
    ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::Rustls)
}

fn roots(ca_pem: &str) -> Result<ureq::tls::RootCerts> {
    let ca = ureq::tls::Certificate::from_pem(ca_pem.as_bytes())?;
    Ok(ureq::tls::RootCerts::new_with_certs(&[ca]))
}

/// The body of an answer, or the coordinator's error as text.
fn body(mut r: ureq::http::Response<ureq::Body>) -> Result<String> {
    let status = r.status();
    let text = r.body_mut().read_to_string()?;
    if status.is_success() {
        Ok(text)
    } else {
        Err(MemberError::Other(format!("{status}: {}", text.trim())))
    }
}

fn json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| MemberError::Other(format!("unexpected answer: {e}")))
}

/// This machine's membership, in `dir`.
pub struct Local {
    dir: PathBuf,
}

impl Local {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, f: &str) -> PathBuf {
        self.dir.join(f)
    }

    pub fn membership(&self) -> Result<Option<Membership>> {
        let p = self.path("member.json");
        match std::fs::read(&p) {
            Ok(b) => Ok(Some(json(&String::from_utf8_lossy(&b))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MemberError::Io(p, e)),
        }
    }

    fn save(&self, m: &Membership) -> Result<()> {
        let p = self.path("member.json");
        std::fs::write(
            &p,
            serde_json::to_vec_pretty(m).expect("membership serialises"),
        )
        .map_err(io(&p))
    }

    /// Ask `coordinator` to admit this machine, after checking that its CA is
    /// the one with `fingerprint`. A machine that asked before asks again with
    /// the same key, so the request is the same; joining another pool starts
    /// over with a new key.
    pub fn join(&self, coordinator: &str, fingerprint: &str, name: &str) -> Result<Membership> {
        let coordinator = coordinator.trim_end_matches('/').to_string();
        let fp = normalise_fingerprint(fingerprint);
        if fp.len() != 64 {
            return Err(MemberError::Other(
                "the fingerprint is the coordinator's 64 hex digits (`devstack node fingerprint` on it)".into(),
            ));
        }
        std::fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;

        // The CA, over a connection that proves nothing by itself: the
        // fingerprint is what makes it trustworthy.
        let blind = agent(tls_builder().disable_verification(true).build());
        let ca = body(blind.get(&format!("{coordinator}/v1/ca")).call()?)?;
        let got = pki::fingerprint(&ca)?;
        if got != fp {
            return Err(MemberError::Other(format!(
                "the coordinator's CA has the fingerprint {got}, not {fp}: not joining"
            )));
        }

        let same_pool = self
            .membership()?
            .filter(|m| m.ca_fingerprint == fp && self.path("member.key").exists());
        let (id, csr) = match same_pool {
            Some(m) => (m.id, m.csr),
            None => {
                let (key, csr) = pki::member_request(name)?;
                write_private(&self.path("member.key"), &key)?;
                let _ = std::fs::remove_file(self.path("member.pem"));
                (pki::member_id(&csr)?, csr)
            }
        };
        write_private(&self.path("ca.pem"), &ca)?;

        let verified = agent(tls_builder().root_certs(roots(&ca)?).build());
        let a: JoinAnswer = json(&body(
            verified
                .post(&format!("{coordinator}/v1/join"))
                .header("Content-Type", "application/json")
                .send(
                    serde_json::to_string(&JoinRequest {
                        name: name.into(),
                        csr: csr.clone(),
                    })
                    .expect("a request serialises"),
                )?,
        )?)?;
        if a.id != id {
            return Err(MemberError::Other(format!(
                "the coordinator answered for `{}`, not `{id}`",
                a.id
            )));
        }
        let m = Membership {
            coordinator,
            ca_fingerprint: fp,
            id,
            name: name.into(),
            csr,
            state: a.state,
        };
        self.take(&m, a)
    }

    /// Store what the coordinator answered about this machine's request.
    fn take(&self, m: &Membership, a: JoinAnswer) -> Result<Membership> {
        let mut m = m.clone();
        m.state = a.state;
        if let Some(cert) = a.cert.filter(|_| a.state == State::Approved) {
            write_private(&self.path("member.pem"), &cert)?;
        }
        self.save(&m)?;
        Ok(m)
    }

    /// Ask the coordinator again whether this machine is admitted.
    pub fn refresh(&self) -> Result<Membership> {
        let m = self.need()?;
        let ca = std::fs::read_to_string(self.path("ca.pem")).map_err(io(&self.path("ca.pem")))?;
        let verified = agent(tls_builder().root_certs(roots(&ca)?).build());
        let a: JoinAnswer = json(&body(
            verified
                .get(&format!("{}/v1/join/{}", m.coordinator, m.id))
                .call()?,
        )?)?;
        self.take(&m, a)
    }

    fn need(&self) -> Result<Membership> {
        self.membership()?.ok_or_else(|| {
            MemberError::Other(
                "this machine is in no pool: `devstack join <coordinator>` first".into(),
            )
        })
    }

    /// An agent that talks to the pool as this member.
    pub fn client(&self) -> Result<(Membership, ureq::Agent)> {
        let m = self.need()?;
        let read = |f: &str| {
            let p = self.path(f);
            std::fs::read_to_string(&p).map_err(|e| MemberError::Io(p, e))
        };
        if m.state != State::Approved || !self.path("member.pem").exists() {
            return Err(MemberError::Other(format!(
                "this machine is {} in the pool, not approved: on the coordinator, `devstack node members approve {}`",
                m.state.as_str(),
                m.id
            )));
        }
        let cert = ureq::tls::Certificate::from_pem(read("member.pem")?.as_bytes())?;
        let key = ureq::tls::PrivateKey::from_pem(read("member.key")?.as_bytes())?;
        let tls = tls_builder()
            .root_certs(roots(&read("ca.pem")?)?)
            .client_cert(Some(ureq::tls::ClientCert::new_with_certs(&[cert], key)))
            .build();
        Ok((m, agent(tls)))
    }

    pub fn me(&self) -> Result<MemberView> {
        let (m, a) = self.client()?;
        json(&body(a.get(&format!("{}/v1/me", m.coordinator)).call()?)?)
    }

    pub fn members(&self) -> Result<Vec<MemberView>> {
        let (m, a) = self.client()?;
        json(&body(
            a.get(&format!("{}/v1/members", m.coordinator)).call()?,
        )?)
    }

    pub fn nodes(&self) -> Result<Vec<NodeView>> {
        let (m, a) = self.client()?;
        json(&body(
            a.get(&format!("{}/v1/nodes", m.coordinator)).call()?,
        )?)
    }

    pub fn report(&self, r: &Report) -> Result<()> {
        let (m, a) = self.client()?;
        body(
            a.post(&format!("{}/v1/nodes/report", m.coordinator))
                .header("Content-Type", "application/json")
                .send(serde_json::to_string(r).expect("a report serialises"))?,
        )?;
        Ok(())
    }
}
