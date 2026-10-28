//! TLS between the pool's machines (ADR-0027, ADR-0031): servers that know
//! their caller by its client certificate, and a client that knows a node by
//! the member id in its certificate, not by the address it is reached at.

use crate::pki;
use axum::{Extension, Router};
use http_body_util::BodyExt;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("tls: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("{0}")]
    Pki(#[from] pki::PkiError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, TlsError>;

pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn roots(ca_pem: &str) -> Result<Arc<rustls::RootCertStore>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(pki::first_der(ca_pem)?))?;
    Ok(Arc::new(roots))
}

fn chain_and_key(
    cert_pem: &str,
    key_pem: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let pem = |e: rustls_pki_types::pem::Error| TlsError::Other(format!("PEM: {e:?}"));
    let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(pem)?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(pem)?;
    Ok((chain, key))
}

/// A server for the pool: `cert_pem`/`key_pem` its own, clients verified
/// against the pool's CA. `require_client`: refuse a connection without a
/// client certificate (a node); else let it in and decide per request (the
/// coordinator, where joining needs none).
pub fn server_config(
    ca_pem: &str,
    cert_pem: &str,
    key_pem: &str,
    require_client: bool,
) -> Result<rustls::ServerConfig> {
    let builder =
        rustls::server::WebPkiClientVerifier::builder_with_provider(roots(ca_pem)?, provider());
    let builder = if require_client {
        builder
    } else {
        builder.allow_unauthenticated()
    };
    let verifier = builder
        .build()
        .map_err(|e| TlsError::Other(format!("client verifier: {e}")))?;
    let (chain, key) = chain_and_key(cert_pem, key_pem)?;
    Ok(rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)?)
}

/// A client for the pool: servers verified against the pool's CA, this
/// member's certificate presented.
pub fn client_config(ca_pem: &str, cert_pem: &str, key_pem: &str) -> Result<rustls::ClientConfig> {
    let (chain, key) = chain_and_key(cert_pem, key_pem)?;
    Ok(rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots(ca_pem)?)
        .with_client_auth_cert(chain, key)?)
}

/// Who is calling: the fingerprint of the client certificate it presented,
/// the member id that certificate was issued to, and where the connection
/// came from.
#[derive(Clone, Debug)]
pub struct Peer {
    pub fp: Option<String>,
    pub member: Option<String>,
    pub addr: std::net::SocketAddr,
}

/// How many connections a pool server holds at once: lanes' tunnels and
/// streams are long-lived, so the limit is generous.
const MAX_CONNECTIONS: usize = 1024;
/// How long a connection may take to send a request's head.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Serve `app` over TLS on `listener` until `stop` resolves; every request
/// carries its connection's [`Peer`] as an extension.
pub async fn serve(
    config: rustls::ServerConfig,
    listener: tokio::net::TcpListener,
    app: Router,
    stop: impl std::future::Future<Output = ()>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    // Connections at once: past this, the next waits until one closes.
    let open = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(stop);
    loop {
        let permit = tokio::select! {
            p = open.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => return,
            },
            () = &mut stop => return,
        };
        let (tcp, addr) = tokio::select! {
            a = listener.accept() => match a {
                Ok(a) => a,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut stop => return,
        };
        // Small writes go out at once: a tunnel's request/answer turns would
        // otherwise wait for delayed ACKs (about 40 ms each).
        let _ = tcp.set_nodelay(true);
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await
            else {
                return;
            };
            let leaf = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|c| c.to_vec());
            let peer = Peer {
                fp: leaf.as_deref().map(pki::fingerprint_der),
                member: leaf.as_deref().and_then(pki::member_of),
                addr,
            };
            let svc = hyper_util::service::TowerToHyperService::new(app.layer(Extension(peer)));
            // A request's head arrives within this, or the connection goes:
            // a caller that sends nothing holds nothing for long.
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT)
                .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                .with_upgrades()
                .await;
        });
    }
}

/// An answer: status and body.
pub struct Answer {
    pub status: u16,
    pub body: bytes::Bytes,
}

/// One request to the member `member` (a node) at `api` (`https://host:port`),
/// over a connection that proves it is that member.
pub async fn request(
    config: Arc<rustls::ClientConfig>,
    api: &str,
    member: &str,
    method: &str,
    path: &str,
    body: Option<(&'static str, bytes::Bytes)>,
) -> Result<Answer> {
    let authority = api
        .strip_prefix("https://")
        .ok_or_else(|| TlsError::Other(format!("`{api}` is no https:// address")))?
        .trim_end_matches('/');
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(authority),
    )
    .await
    .map_err(|_| TlsError::Other(format!("{authority}: no answer within 10 s")))?
    .map_err(|e| TlsError::Other(format!("{authority}: {e}")))?;
    let _ = tcp.set_nodelay(true);
    let name = ServerName::try_from(pki::member_dns(member))
        .map_err(|e| TlsError::Other(format!("server name: {e}")))?;
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(|e| TlsError::Other(format!("{authority}: TLS: {e}")))?;
    let (mut send, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
        .await
        .map_err(|e| TlsError::Other(format!("{authority}: {e}")))?;
    tokio::spawn(conn);
    // What sends it is a devstack daemon, the lane's requester: the lane
    // daemon's journal says "by devstack", not "an unknown client"
    // (ADR-0067). The client a person used is in the requester's journal.
    let mut req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(hyper::header::HOST, authority)
        .header("x-devstack-client", "auto");
    let payload = match body {
        Some((ty, b)) => {
            req = req.header(hyper::header::CONTENT_TYPE, ty);
            b
        }
        None => bytes::Bytes::new(),
    };
    let req = req
        .body(http_body_util::Full::new(payload))
        .map_err(|e| TlsError::Other(e.to_string()))?;
    let res = tokio::time::timeout(Duration::from_secs(120), send.send_request(req))
        .await
        .map_err(|_| TlsError::Other(format!("{authority}{path}: no answer within 120 s")))?
        .map_err(|e| TlsError::Other(format!("{authority}{path}: {e}")))?;
    let status = res.status().as_u16();
    let body = res
        .into_body()
        .collect()
        .await
        .map_err(|e| TlsError::Other(format!("{authority}{path}: {e}")))?
        .to_bytes();
    Ok(Answer { status, body })
}

/// Compare two secrets in constant time.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |x, (p, q)| x | (p ^ q)) == 0
}

/// Send `req` (path and query in its URI) to the member `member` at `api` and
/// hand back the answer as it arrives: streams stay streams.
pub async fn send<B>(
    config: Arc<rustls::ClientConfig>,
    api: &str,
    member: &str,
    mut req: hyper::Request<B>,
) -> Result<hyper::Response<hyper::body::Incoming>>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let authority = api
        .strip_prefix("https://")
        .ok_or_else(|| TlsError::Other(format!("`{api}` is no https:// address")))?
        .trim_end_matches('/')
        .to_string();
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(&authority),
    )
    .await
    .map_err(|_| TlsError::Other(format!("{authority}: no answer within 10 s")))?
    .map_err(|e| TlsError::Other(format!("{authority}: {e}")))?;
    let _ = tcp.set_nodelay(true);
    let name = ServerName::try_from(pki::member_dns(member))
        .map_err(|e| TlsError::Other(format!("server name: {e}")))?;
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(|e| TlsError::Other(format!("{authority}: TLS: {e}")))?;
    let (mut s, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
        .await
        .map_err(|e| TlsError::Other(format!("{authority}: {e}")))?;
    // Upgrades too: a tunnel to a lane's port is one (ADR-0029 point 4).
    tokio::spawn(conn.with_upgrades());
    req.headers_mut().insert(
        hyper::header::HOST,
        hyper::header::HeaderValue::from_str(&authority)
            .map_err(|e| TlsError::Other(e.to_string()))?,
    );
    s.send_request(req)
        .await
        .map_err(|e| TlsError::Other(format!("{authority}: {e}")))
}
