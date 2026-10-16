//! A machine's membership in a pool (ADR-0031 point 2): its key, its request
//! to join, and once approved its certificate, in its own directory. Every
//! call to the coordinator after the first is mutual TLS with the pool's CA.

use crate::coordinator::{
    ApproveRequest, JoinAnswer, JoinRequest, LeaseOffer, LeaseRequest, MemberView, NodeView,
    Report, RoleRequest,
};
use crate::pki;
use crate::store::{AuditEntry, Role, State};
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
    /// The coordinator answered, with an error.
    #[error("{0}: {1}")]
    Status(u16, String),
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
        Err(MemberError::Status(
            status.as_u16(),
            text.trim().to_string(),
        ))
    }
}

fn json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| MemberError::Other(format!("unexpected answer: {e}")))
}

/// Where this machine's membership lives: `DEVSTACK_POOL_DIR`, else `pool/`
/// in devstack's per-user state `state` (the CLI and the hub share it).
pub fn default_dir(state: &Path) -> PathBuf {
    std::env::var_os("DEVSTACK_POOL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| state.join("pool"))
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

    /// `GET` a coordinator path as this member.
    pub fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (m, a) = self.client()?;
        json(&body(a.get(&format!("{}{path}", m.coordinator)).call()?)?)
    }

    /// `POST` JSON to a coordinator path as this member; an empty answer
    /// reads as `null`.
    pub fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        payload: &impl Serialize,
    ) -> Result<T> {
        let (m, a) = self.client()?;
        let text = body(
            a.post(&format!("{}{path}", m.coordinator))
                .header("Content-Type", "application/json")
                .send(serde_json::to_string(payload).expect("a request serialises"))?,
        )?;
        json(if text.trim().is_empty() {
            "null"
        } else {
            &text
        })
    }

    /// The pool's CA, this member's certificate and key, PEM (for a client
    /// that talks to a node).
    pub fn pems(&self) -> Result<(String, String, String)> {
        self.client()?;
        let read = |f: &str| {
            let p = self.path(f);
            std::fs::read_to_string(&p).map_err(|e| MemberError::Io(p, e))
        };
        Ok((read("ca.pem")?, read("member.pem")?, read("member.key")?))
    }

    /// Ask the coordinator for a lane on a node (ADR-0031 point 4).
    pub fn lease(&self, r: &LeaseRequest) -> Result<LeaseOffer> {
        self.post("/v1/leases", r)
    }

    pub fn me(&self) -> Result<MemberView> {
        self.get("/v1/me")
    }

    pub fn members(&self) -> Result<Vec<MemberView>> {
        self.get("/v1/members")
    }

    pub fn nodes(&self) -> Result<Vec<NodeView>> {
        self.get("/v1/nodes")
    }

    /// Report to the coordinator; its answer says what the node is to do.
    pub fn report(&self, r: &Report) -> Result<crate::coordinator::ReportAnswer> {
        let v: Option<crate::coordinator::ReportAnswer> = self.post("/v1/nodes/report", r)?;
        Ok(v.unwrap_or_default())
    }

    /// The leases of the pool: which lane runs where.
    pub fn leases(&self) -> Result<Vec<crate::coordinator::Lease>> {
        self.get("/v1/leases")
    }

    /// Pause a node, or let it take lanes again (admins only).
    pub fn admin_pause(&self, id: &str, paused: bool) -> Result<MemberView> {
        let what = if paused { "pause" } else { "resume" };
        self.post(&format!("/v1/admin/nodes/{id}/{what}"), &())
    }

    /// Set a member's tags (admins only, ADR-0033).
    pub fn admin_tags(&self, id: &str, tags: &[String]) -> Result<MemberView> {
        self.post(
            &format!("/v1/admin/members/{id}/tags"),
            &crate::coordinator::TagsRequest {
                tags: tags.to_vec(),
            },
        )
    }

    /// Set what a node lets run on it; `None` lifts it (admins only).
    pub fn admin_policy(
        &self,
        id: &str,
        policy: Option<crate::policy::NodePolicy>,
    ) -> Result<MemberView> {
        self.post(&format!("/v1/admin/nodes/{id}/policy"), &policy)
    }

    /// End a lease (admins only): its node stops the lane.
    pub fn admin_end(&self, lease: &str) -> Result<()> {
        self.post::<serde_json::Value>(&format!("/v1/admin/leases/{lease}/end"), &())?;
        Ok(())
    }

    /// Every member with the address it asked from (admins only, ADR-0032).
    pub fn admin_members(&self) -> Result<Vec<MemberView>> {
        self.get("/v1/admin/members")
    }

    pub fn admin_approve(&self, id: &str, role: Option<Role>) -> Result<MemberView> {
        self.post(
            &format!("/v1/admin/members/{id}/approve"),
            &ApproveRequest { role },
        )
    }

    /// Remove a member, or reject its request.
    pub fn admin_remove(&self, id: &str) -> Result<MemberView> {
        self.post(&format!("/v1/admin/members/{id}/remove"), &())
    }

    pub fn admin_role(&self, id: &str, role: Role) -> Result<MemberView> {
        self.post(
            &format!("/v1/admin/members/{id}/role"),
            &RoleRequest { role },
        )
    }

    pub fn admin_audit(&self) -> Result<Vec<AuditEntry>> {
        self.get("/v1/admin/audit")
    }
}
