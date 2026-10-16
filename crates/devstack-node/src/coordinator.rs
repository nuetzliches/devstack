//! The coordinator (ADR-0031): its certificate authority, the members and
//! what the nodes report, served over TLS. A request is the member whose
//! client certificate it presents; joining needs none.
//!
//! - `GET /v1/ca`: the pool's CA (a joining member compares its fingerprint)
//! - `POST /v1/join` `{name, csr}`: ask to join; `GET /v1/join/{id}`: the answer
//! - members only: `GET /v1/me`, `GET /v1/members`, `POST /v1/nodes/report`,
//!   `GET /v1/nodes`
//! - admins only (ADR-0032): `GET /v1/admin/members`,
//!   `POST /v1/admin/members/{id}/approve` `{role?}`,
//!   `POST /v1/admin/members/{id}/remove` (also rejects a request),
//!   `POST /v1/admin/members/{id}/role` `{role}`, `GET /v1/admin/audit`

use crate::pki::{self, Pair};
use crate::store::{self, AuditEntry, Member, Role, State, Store};
use axum::extract::{Path as UrlPath, State as AxState};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A node that has not reported for this long is offline.
pub const ONLINE_FOR: Duration = Duration::from_secs(45);

#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}")]
    Pki(#[from] pki::PkiError),
    #[error("{0}")]
    Store(#[from] store::StoreError),
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoordError>;

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| CoordError::Io(path.into(), e))
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(|e| CoordError::Io(path.into(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CoordError::Io(path.into(), e))?;
    }
    Ok(())
}

/// Create a coordinator in `dir`: a new CA and a server certificate for
/// `names` (the addresses and host names members reach it at). Returns the
/// CA's fingerprint, which a member passes to `devstack join`.
pub fn init(dir: &Path, names: &[String]) -> Result<String> {
    if names.is_empty() {
        return Err(CoordError::Other(
            "name at least one address or host name members reach the coordinator at".into(),
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| CoordError::Io(dir.into(), e))?;
    if dir.join("ca.key").exists() {
        return Err(CoordError::Other(format!(
            "{} already holds a coordinator; its CA stays (a new one would lock every member out)",
            dir.display()
        )));
    }
    let ca = pki::new_ca()?;
    let server = pki::server_cert(&ca, names)?;
    write_private(&dir.join("ca.key"), &ca.key)?;
    write_private(&dir.join("ca.pem"), &ca.cert)?;
    write_private(&dir.join("server.key"), &server.key)?;
    write_private(&dir.join("server.pem"), &server.cert)?;
    let names = serde_json::to_string_pretty(&serde_json::json!({ "names": names }))
        .expect("names serialise");
    write_private(&dir.join("coordinator.json"), &names)?;
    Ok(pki::fingerprint(&ca.cert)?)
}

/// The addresses and host names the coordinator in `dir` was made for.
pub fn names(dir: &Path) -> Result<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&read(&dir.join("coordinator.json"))?)
        .map_err(|e| CoordError::Other(format!("coordinator.json: {e}")))?;
    Ok(v["names"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str().map(str::to_string))
        .collect())
}

/// The CA of the coordinator in `dir`.
pub fn ca(dir: &Path) -> Result<Pair> {
    Ok(Pair {
        cert: read(&dir.join("ca.pem"))?,
        key: read(&dir.join("ca.key"))?,
    })
}

/// What a node reports about itself (ADR-0031 point 3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// Whether its owner shares it (the switch in the dashboard).
    #[serde(default)]
    pub sharing: bool,
    /// CPU cores it offers now.
    #[serde(default)]
    pub cpus: f64,
    /// Memory it offers now, bytes.
    #[serde(default)]
    pub memory: u64,
    /// Where its node API listens (an address on the pool's network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
}

/// A member as the pool shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub online: bool,
    /// Seconds since its last report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
}

/// A member as `/v1/members` lists it (no request or certificate); the
/// address it asked from only for admins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub state: State,
    #[serde(default)]
    pub role: Role,
    pub requested_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

impl From<&Member> for MemberView {
    fn from(m: &Member) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            state: m.state,
            role: m.role,
            requested_at: m.requested_at,
            address: None,
        }
    }
}

impl MemberView {
    fn for_admin(m: &Member) -> Self {
        Self {
            address: m.address.clone(),
            ..Self::from(m)
        }
    }
}

/// `POST /v1/admin/members/{id}/approve`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApproveRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

/// `POST /v1/admin/members/{id}/role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleRequest {
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub name: String,
    pub csr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinAnswer {
    pub id: String,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
}

struct Inner {
    dir: PathBuf,
    store: Store,
    ca_pem: String,
    reports: Mutex<HashMap<String, (std::time::Instant, Report)>>,
}

/// The fingerprint of the client certificate a connection presented, and
/// where the connection came from.
#[derive(Clone)]
struct Peer {
    fp: Option<String>,
    addr: std::net::SocketAddr,
}

type Shared = Arc<Inner>;
type ApiResult<T> = std::result::Result<T, (StatusCode, String)>;

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// The approved member behind a request.
fn member(s: &Inner, peer: &Peer) -> ApiResult<Member> {
    let Some(fp) = &peer.fp else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "no client certificate: join the pool first (`devstack join`)".into(),
        ));
    };
    s.store.by_cert(fp).map_err(internal)?.ok_or((
        StatusCode::FORBIDDEN,
        "this certificate is not an approved member of the pool".into(),
    ))
}

/// The approved admin behind a request, and how the audit log names them.
fn admin(s: &Inner, peer: &Peer) -> ApiResult<(Member, String)> {
    let m = member(s, peer)?;
    if m.role != Role::Admin {
        return Err((
            StatusCode::FORBIDDEN,
            "only an admin of the pool may do this".into(),
        ));
    }
    let by = format!("{} ({})", m.id, m.name);
    Ok((m, by))
}

/// A store error as an answer: unknown and ambiguous members are the
/// caller's mistake, a refused change a conflict.
fn store_err(e: store::StoreError) -> (StatusCode, String) {
    use store::StoreError as E;
    let status = match &e {
        E::Unknown(_) => StatusCode::NOT_FOUND,
        E::Ambiguous(..) => StatusCode::BAD_REQUEST,
        E::Refused(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, e.to_string())
}

fn router(shared: Shared) -> Router {
    Router::new()
        .route(
            "/v1/ca",
            get(|AxState(s): AxState<Shared>| async move { s.ca_pem.clone() }),
        )
        .route("/v1/join", post(join))
        .route("/v1/join/{id}", get(join_state))
        .route("/v1/me", get(me))
        .route("/v1/members", get(members))
        .route("/v1/nodes/report", post(report))
        .route("/v1/nodes", get(nodes))
        .route("/v1/admin/members", get(admin_members))
        .route("/v1/admin/members/{id}/approve", post(admin_approve))
        .route("/v1/admin/members/{id}/remove", post(admin_remove))
        .route("/v1/admin/members/{id}/role", post(admin_role))
        .route("/v1/admin/audit", get(admin_audit))
        .with_state(shared)
}

async fn join(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<JoinRequest>,
) -> ApiResult<Json<JoinAnswer>> {
    let id = pki::member_id(&r.csr).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let name: String = r
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let m = s
        .store
        .request(&id, name.trim(), &r.csr, Some(&peer.addr.ip().to_string()))
        .map_err(internal)?;
    Ok(Json(answer(&m)))
}

fn answer(m: &Member) -> JoinAnswer {
    JoinAnswer {
        id: m.id.clone(),
        state: m.state,
        cert: m.cert.clone().filter(|_| m.state == State::Approved),
    }
}

async fn join_state(
    AxState(s): AxState<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<JoinAnswer>> {
    match s.store.get(&id).map_err(internal)? {
        Some(m) => Ok(Json(answer(&m))),
        None => Err((StatusCode::NOT_FOUND, format!("no request `{id}`"))),
    }
}

async fn me(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<MemberView>> {
    Ok(Json(MemberView::from(&member(&s, &peer)?)))
}

async fn members(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<MemberView>>> {
    member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    Ok(Json(ms.iter().map(MemberView::from).collect()))
}

async fn report(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<Report>,
) -> ApiResult<StatusCode> {
    let m = member(&s, &peer)?;
    s.reports
        .lock()
        .unwrap()
        .insert(m.id, (std::time::Instant::now(), r));
    Ok(StatusCode::NO_CONTENT)
}

async fn nodes(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<NodeView>>> {
    member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    let reports = s.reports.lock().unwrap();
    Ok(Json(
        ms.iter()
            .filter(|m| m.state == State::Approved)
            .map(|m| {
                let r = reports.get(&m.id);
                let age = r.map(|(at, _)| at.elapsed());
                NodeView {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    online: age.is_some_and(|a| a < ONLINE_FOR),
                    seen: age.map(|a| a.as_secs()),
                    report: r.map(|(_, r)| r.clone()),
                }
            })
            .collect(),
    ))
}

async fn admin_members(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<MemberView>>> {
    admin(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    Ok(Json(ms.iter().map(MemberView::for_admin).collect()))
}

async fn admin_approve(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<ApproveRequest>>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let role = body.and_then(|Json(b)| b.role);
    let ca = ca(&s.dir).map_err(internal)?;
    let m = s.store.approve(&id, &ca, role, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_remove(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.remove(&id, &by).map_err(store_err)?;
    s.reports.lock().unwrap().remove(&m.id);
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_role(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(r): Json<RoleRequest>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_role(&id, r.role, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_audit(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    admin(&s, &peer)?;
    Ok(Json(s.store.audit(200).map_err(internal)?))
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_config(dir: &Path) -> Result<rustls::ServerConfig> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let pem_err = |e: rustls_pki_types::pem::Error| CoordError::Other(format!("PEM: {e:?}"));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(pki::first_der(&read(
        &dir.join("ca.pem"),
    )?)?))?;
    let verifier =
        rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
            .allow_unauthenticated()
            .build()
            .map_err(|e| CoordError::Other(format!("client verifier: {e}")))?;
    let chain = CertificateDer::pem_slice_iter(read(&dir.join("server.pem"))?.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(pem_err)?;
    let key = PrivateKeyDer::from_pem_slice(read(&dir.join("server.key"))?.as_bytes())
        .map_err(pem_err)?;
    Ok(rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)?)
}

/// Serve the coordinator in `dir` on `listener` until `stop` resolves.
pub async fn serve(
    dir: &Path,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config(dir)?));
    let shared = Arc::new(Inner {
        dir: dir.to_path_buf(),
        store: Store::new(dir),
        ca_pem: read(&dir.join("ca.pem"))?,
        reports: Mutex::new(HashMap::new()),
    });
    let app = router(shared);
    tokio::pin!(stop);
    loop {
        let (tcp, addr) = tokio::select! {
            a = listener.accept() => match a {
                Ok(a) => a,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut stop => return Ok(()),
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await
            else {
                return;
            };
            let fp = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|c| pki::fingerprint_der(c));
            let svc = hyper_util::service::TowerToHyperService::new(
                app.layer(Extension(Peer { fp, addr })),
            );
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                .await;
        });
    }
}

/// Serve the coordinator in `dir` on `listen` until Ctrl-C (or SIGTERM).
pub fn serve_blocking(dir: &Path, listen: std::net::SocketAddr) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CoordError::Other(e.to_string()))?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|e| CoordError::Other(format!("listen on {listen}: {e}")))?;
        serve(dir, listener, stop_signal()).await
    })
}

async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
