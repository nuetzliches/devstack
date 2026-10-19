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
    pki::write_private(path, text).map_err(io(path))
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

/// The pool a machine joins first, and the one meant where none is named
/// (ADR-0034).
pub const DEFAULT_POOL: &str = "default";

/// Is `name` usable as a pool's name: `a-z`, `0-9`, `-`, `_`, at most 32?
pub fn valid_pool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Where this machine's membership of the pool `name` lives: the default
/// pool's in [`default_dir`], every other one's in `pools/<name>` beside it
/// (ADR-0034: a membership per pool).
pub fn pool_dir(state: &Path, name: &str) -> PathBuf {
    let default = default_dir(state);
    if name == DEFAULT_POOL {
        return default;
    }
    default
        .parent()
        .map_or_else(|| state.to_path_buf(), Path::to_path_buf)
        .join("pools")
        .join(name)
}

/// The pools this machine belongs to or asked to join: the default pool
/// first (always, joined or not), then the others by name.
pub fn pools(state: &Path) -> Vec<(String, PathBuf)> {
    let mut out = vec![(DEFAULT_POOL.to_string(), default_dir(state))];
    let others = pool_dir(state, "x")
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut names: Vec<String> = std::fs::read_dir(&others)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("member.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != DEFAULT_POOL && valid_pool_name(n))
        .collect();
    names.sort();
    out.extend(names.into_iter().map(|n| {
        let d = others.join(&n);
        (n, d)
    }));
    out
}

/// Where a lane is to run, as `--on` names it (ADR-0034 point 3), split
/// into the pool and what its coordinator is asked for (a node, or `pool`
/// for its choice):
///
/// - `<pool>:<node>`, `<pool>:` or `<pool>:pool`: in that pool;
/// - `<pool>` alone, a pool this machine belongs to other than the default
///   one: that pool's coordinator chooses;
/// - anything else (`pool`, a node's name or id): the default pool.
pub fn resolve_on(state: &Path, on: &str) -> std::result::Result<(String, String), String> {
    let on = on.trim();
    let known = pools(state);
    let have = |p: &str| known.iter().any(|(n, _)| n == p);
    if let Some((pool, node)) = on.split_once(':') {
        if !have(pool) {
            return Err(format!(
                "this machine is in no pool `{pool}` (`devstack join <coordinator> --pool {pool}`)"
            ));
        }
        let node = if node.is_empty() { "pool" } else { node };
        return Ok((pool.to_string(), node.to_string()));
    }
    if on != DEFAULT_POOL && !on.eq_ignore_ascii_case("pool") && have(on) {
        return Ok((on.to_string(), "pool".into()));
    }
    if on == DEFAULT_POOL {
        return Ok((DEFAULT_POOL.into(), "pool".into()));
    }
    Ok((DEFAULT_POOL.into(), on.to_string()))
}

/// This machine's membership, in `dir`.
pub struct Local {
    dir: PathBuf,
}

impl Local {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.into() }
    }

    /// Where the membership lives.
    pub fn dir(&self) -> &Path {
        &self.dir
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

    /// `PUT` JSON to a coordinator path as this member; the answer is not
    /// read.
    pub fn put(&self, path: &str, payload: &impl Serialize) -> Result<()> {
        let (m, a) = self.client()?;
        body(
            a.put(&format!("{}{path}", m.coordinator))
                .header("Content-Type", "application/json")
                .send(serde_json::to_string(payload).expect("a request serialises"))?,
        )?;
        Ok(())
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

    /// Share a lane this machine leased, or stop sharing it (`None`,
    /// ADR-0035).
    pub fn share_lease(
        &self,
        lease: &str,
        sharing: Option<&crate::coordinator::Sharing>,
    ) -> Result<()> {
        self.put(&format!("/v1/leases/{lease}/share"), &sharing)
    }

    /// The lanes others share with this machine.
    pub fn shared(&self) -> Result<Vec<crate::coordinator::SharedLane>> {
        self.get("/v1/shared")
    }

    /// Whom a lease on this node is shared with (the node asks).
    pub fn lease_shared(&self, lease: &str) -> Result<Option<crate::coordinator::Sharing>> {
        self.get(&format!("/v1/leases/{lease}/shared"))
    }

    /// A node's capacity over time, or the pool's (`node` none); `day`: the
    /// last 24 hours by minute, else the last hour (ADR-0036).
    pub fn history(&self, node: Option<&str>, day: bool) -> Result<Vec<crate::coordinator::Point>> {
        let range = if day { "24h" } else { "1h" };
        match node {
            Some(n) => self.get(&format!("/v1/history?node={n}&range={range}")),
            None => self.get(&format!("/v1/history?range={range}")),
        }
    }

    /// Whether the pool collects telemetry (ADR-0037).
    pub fn telemetry_info(&self) -> Result<crate::telemetry::Info> {
        self.get("/v1/telemetry")
    }

    /// Send OTLP/JSON (`logs` or `metrics`) to the pool's collector, through
    /// the coordinator.
    pub fn telemetry_send(&self, kind: &str, body: &serde_json::Value) -> Result<()> {
        let _: serde_json::Value = self.post(&format!("/v1/telemetry/{kind}"), body)?;
        Ok(())
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

    /// Grant a member to run the pool's lanes, or take it back (admins
    /// only).
    pub fn admin_node(&self, id: &str, node: bool) -> Result<MemberView> {
        self.post(
            &format!("/v1/admin/members/{id}/node"),
            &crate::coordinator::NodeRequest { node },
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
    /// Have server `id` update its devstack (admins only, ADR-0039).
    pub fn admin_update(&self, id: &str, req: &crate::coordinator::UpdateRequest) -> Result<()> {
        self.post::<serde_json::Value>(&format!("/v1/admin/nodes/{id}/update"), req)?;
        Ok(())
    }

    /// Have node `id` remove the stopped lanes it keeps (admins only,
    /// ADR-0038).
    pub fn admin_prune(&self, id: &str) -> Result<()> {
        self.post::<serde_json::Value>(&format!("/v1/admin/nodes/{id}/prune"), &())?;
        Ok(())
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_names_a_pool_and_a_node() {
        if std::env::var_os("DEVSTACK_POOL_DIR").is_some() {
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let acme = pool_dir(state.path(), "acme");
        assert_eq!(acme, state.path().join("pools").join("acme"));
        assert_eq!(
            pool_dir(state.path(), DEFAULT_POOL),
            state.path().join("pool")
        );
        std::fs::create_dir_all(&acme).unwrap();
        std::fs::write(acme.join("member.json"), "{}").unwrap();
        let names: Vec<String> = pools(state.path()).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["default", "acme"]);
        let r = |on: &str| resolve_on(state.path(), on);
        let pair = |p: &str, n: &str| Ok((p.to_string(), n.to_string()));
        assert_eq!(r("build-server"), pair("default", "build-server"));
        assert_eq!(r("pool"), pair("default", "pool"));
        assert_eq!(r("default"), pair("default", "pool"));
        assert_eq!(r("acme"), pair("acme", "pool"));
        assert_eq!(r("acme:gpu-1"), pair("acme", "gpu-1"));
        assert_eq!(r("acme:"), pair("acme", "pool"));
        assert_eq!(r("default:build-server"), pair("default", "build-server"));
        assert!(r("other:x").is_err());
        assert!(!valid_pool_name("Acme"));
        assert!(!valid_pool_name("../x"));
    }
}
