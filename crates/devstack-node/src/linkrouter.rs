//! What carries public links through a named tunnel (ADR-0091, ADR-0092),
//! shared by the hub (a member's machine) and a node's agent: the one
//! `cloudflared` connector of a tunnel, and the router it sends every host
//! to, which passes only the hosts its owner knows to a service, as a
//! browser on that machine would ask (`Host: localhost:<port>`), WebSockets
//! included. Where a host goes is the owner's [`Dial`]: a port on this
//! machine for the hub, a lane's port through its daemon for a node.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::{Request, StatusCode, header};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

type Body = BoxBody<Bytes, hyper::Error>;

/// How long `cloudflared` gets to register a connection.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(45);
/// A failed connector is started again no sooner than this.
const RETRY: Duration = Duration::from_secs(30);
/// How long a connector runs on after the last link closed.
const GRACE: Duration = Duration::from_secs(20);

pub trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

/// Opens a connection to a service: TCP here, or a lane's tunnel on a node.
pub type Open = Pin<Box<dyn Future<Output = Result<Box<dyn Io>, (u16, String)>> + Send>>;

/// Where an open host goes, as its owner checked it; not connected yet
/// (#559). The router keeps connections to services and reuses them, keyed
/// by `key`, the target, never the host name: a link closed and opened again
/// for another service never gets the old connection, and every request is
/// checked before a kept connection is taken.
pub struct Way {
    /// The target: `local:<port>:<tls>` here, `lane:<lease>:<port>:<tls>` on
    /// a node.
    pub key: String,
    /// Its port, as a browser on its machine names it in `Host`.
    pub port: u16,
    /// It speaks TLS (a development certificate).
    pub tls: bool,
    /// Opens a new connection, when none is kept.
    pub open: Open,
}

/// Where a host goes, or the status and why not. It only checks: it opens
/// nothing (the router does, with [`Way::open`]).
pub type Dial = Arc<dyn Fn(String) -> Result<Way, (u16, String)> + Send + Sync>;

/// Idle connections to services, by [`Way::key`].
type Kept = Arc<crate::connpool::Pool<String, Body>>;
/// One visitor connection's service connection, once a login bound it.
type Client = Arc<crate::connpool::Bound<String, Body>>;

// ------------------------------------------------------- the connector

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ConnState {
    Off,
    Starting,
    Running,
    Failed { error: String },
}

/// A tunnel's one `cloudflared`, started while links want it.
pub struct Connector {
    tree: Option<devstack_proc::Owned>,
    state: ConnState,
    /// Unix ms of the last change of `state`.
    since: u128,
    started: Option<Instant>,
    failed_at: Option<Instant>,
    idle_since: Option<Instant>,
    log: PathBuf,
}

impl Connector {
    /// Its output goes to `log`.
    pub fn new(log: PathBuf) -> Self {
        Self {
            tree: None,
            state: ConnState::Off,
            since: now_ms(),
            started: None,
            failed_at: None,
            idle_since: None,
            log,
        }
    }

    pub fn state(&self) -> &ConnState {
        &self.state
    }

    /// Follow it, start it when `wanted` (`start` makes it), end it a while
    /// after it is no longer, or at once with `off_now`.
    pub fn tick(
        &mut self,
        wanted: bool,
        off_now: bool,
        start: impl FnOnce() -> Result<devstack_proc::Owned, String>,
    ) {
        if let Some(tree) = self.tree.as_mut() {
            if let Some(code) = tree.try_exit_code() {
                let out = std::fs::read_to_string(&self.log).unwrap_or_default();
                let error = cloudflared_error(&out)
                    .unwrap_or_else(|| format!("cloudflared ended (exit code {code})"));
                self.tree = None;
                self.fail(error);
            } else if self.state == ConnState::Starting {
                let out = std::fs::read_to_string(&self.log).unwrap_or_default();
                if out.contains("Registered tunnel connection") {
                    self.set(ConnState::Running);
                } else if self.started.is_some_and(|t| t.elapsed() > OPEN_TIMEOUT) {
                    if let Some(mut t) = self.tree.take() {
                        t.kill();
                    }
                    let error = cloudflared_error(&out).unwrap_or_else(|| {
                        format!(
                            "no connection registered after {} s",
                            OPEN_TIMEOUT.as_secs()
                        )
                    });
                    self.fail(error);
                }
            }
        }
        if wanted && !off_now {
            self.idle_since = None;
            let may_retry = self.failed_at.is_none_or(|t| t.elapsed() > RETRY);
            if self.tree.is_none() && may_retry {
                match start() {
                    Ok(tree) => {
                        self.tree = Some(tree);
                        self.started = Some(Instant::now());
                        self.failed_at = None;
                        self.set(ConnState::Starting);
                    }
                    Err(error) => self.fail(error),
                }
            }
        } else if self.tree.is_some() {
            let idle = *self.idle_since.get_or_insert_with(Instant::now);
            if off_now || idle.elapsed() > GRACE {
                self.stop();
            }
        } else if self.state != ConnState::Off && self.failed_at.is_none() {
            self.set(ConnState::Off);
        }
    }

    /// End it now; a failure is forgotten.
    pub fn stop(&mut self) {
        if let Some(mut t) = self.tree.take() {
            t.kill();
        }
        self.failed_at = None;
        self.set(ConnState::Off);
    }

    /// `{state, error?, since, pid}`.
    pub fn view(&self) -> Value {
        let mut v = serde_json::to_value(&self.state).unwrap_or_default();
        v["since"] = json!(self.since);
        v["pid"] = json!(self.tree.as_ref().map(|t| t.root_pid()));
        v
    }

    fn fail(&mut self, error: String) {
        self.set(ConnState::Failed { error });
        self.failed_at = Some(Instant::now());
    }

    fn set(&mut self, state: ConnState) {
        if self.state != state {
            self.state = state;
            self.since = now_ms();
        }
    }
}

/// `cloudflared tunnel run` for tunnel `tunnel_id`, its credentials in the
/// environment (not on its command line), every host to the router on
/// `router_port`. A config file of its own in `dir`: `cloudflared` would
/// otherwise read `~/.cloudflared/config.yml`, and its ingress rules.
pub fn spawn_connector(
    dir: &Path,
    tunnel_id: &str,
    router_port: u16,
    credentials: String,
    log: &Path,
    opts: &devstack_proc::Options,
) -> Result<devstack_proc::Owned, String> {
    let program = cloudflared()?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = dir.join("cloudflared.yml");
    std::fs::write(
        &file,
        format!(
            "tunnel: {tunnel_id}\nno-autoupdate: true\ningress:\n  - service: http://127.0.0.1:{router_port}\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log).map_err(|e| e.to_string())?;
    let mut env = BTreeMap::new();
    env.insert("TUNNEL_CRED_CONTENTS".to_string(), credentials);
    let (tree, _) = devstack_proc::spawn(
        devstack_proc::Spec {
            name: "public-links".into(),
            command: format!("{} tunnel --config {} run", quoted(&program), quoted(&file)),
            cwd: dir.to_path_buf(),
            env,
            log,
        },
        opts,
    )
    .map_err(|e| e.to_string())?;
    Ok(tree)
}

/// The options to own `cloudflared`'s tree as a service's (ADR-0009): on
/// Unix through devstack's shim.
pub fn proc_options() -> devstack_proc::Options {
    let shim = std::env::current_exe()
        .map(|exe| vec![exe.to_string_lossy().into_owned(), "shim".into()])
        .unwrap_or_default();
    devstack_proc::Options {
        cgroup_root: None,
        shim,
    }
}

// ---------------------------------------------------------- the router

/// Serve the router on `listener`, for the process's life.
pub async fn serve(listener: tokio::net::TcpListener, dial: Dial) {
    let kept: Kept = crate::connpool::Pool::new(crate::connpool::IDLE_MAX);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let _ = stream.set_nodelay(true);
        let dial = dial.clone();
        let kept = kept.clone();
        // The service connection a login binds to this visitor's (#569).
        let client: Client = Arc::new(crate::connpool::Bound::default());
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |r| {
                handle(dial.clone(), kept.clone(), client.clone(), r)
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}

fn text(status: StatusCode, body: String) -> hyper::Response<Body> {
    let mut r = hyper::Response::new(Full::new(Bytes::from(body)).map_err(|n| match n {}).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

async fn handle(
    dial: Dial,
    kept: Kept,
    client: Client,
    req: Request<Incoming>,
) -> Result<hyper::Response<Body>, std::convert::Infallible> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase())
        .unwrap_or_default();
    // Every request is checked, a kept connection or not.
    let way = match dial(host.clone()) {
        Ok(w) => w,
        Err((status, why)) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            return Ok(text(status, format!("devstack: {why}\n")));
        }
    };
    Ok(forward(req, &host, way, kept, &client)
        .await
        .unwrap_or_else(|e| {
            text(
                StatusCode::BAD_GATEWAY,
                format!("devstack: the service does not answer: {e}\n"),
            )
        }))
}

/// The way to `port` on this machine's loopback (IPv4, else IPv6: a dev
/// server may listen on `::1` only).
pub fn local(port: u16, tls: bool) -> Way {
    Way {
        key: format!("local:{port}:{tls}"),
        port,
        tls,
        open: Box::pin(async move {
            let tcp = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                Ok(s) => s,
                Err(_) => tokio::net::TcpStream::connect(("::1", port))
                    .await
                    .map_err(|e| (502, format!("nothing answers on port {port}: {e}")))?,
            };
            let _ = tcp.set_nodelay(true);
            Ok(Box::new(tcp) as Box<dyn Io>)
        }),
    }
}

/// A new HTTP/1.1 connection along `open`, with TLS where the service
/// speaks it.
async fn connect(
    open: Open,
    tls: bool,
) -> Result<hyper::client::conn::http1::SendRequest<Body>, String> {
    let io = open.await.map_err(|(_, why)| why)?;
    let io: Box<dyn Io> = if tls {
        let name =
            rustls_pki_types::ServerName::try_from("localhost").map_err(|e| e.to_string())?;
        Box::new(
            tokio_rustls::TlsConnector::from(any_certificate())
                .connect(name, io)
                .await
                .map_err(|e| e.to_string())?,
        )
    } else {
        io
    };
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn.with_upgrades());
    Ok(sender)
}

/// Forward a request along `way`, with the Host a browser on the service's
/// machine sends; WebSockets pass through. A kept connection to its target
/// is used if there is one; read to its end, the answer's connection is kept
/// for the next request.
async fn forward(
    mut req: Request<Incoming>,
    host: &str,
    way: Way,
    kept: Kept,
    client: &Client,
) -> Result<hyper::Response<Body>, String> {
    let port = way.port;
    let client_upgrade = req
        .headers()
        .contains_key(header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    {
        let h = req.headers_mut();
        strip_hop_by_hop(h, client_upgrade.is_some());
        if let Ok(v) = header::HeaderValue::from_str(&format!("localhost:{port}")) {
            h.insert(header::HOST, v);
        }
        if let Ok(v) = header::HeaderValue::from_str(host) {
            h.insert("x-forwarded-host", v);
        }
        h.insert(
            "x-forwarded-proto",
            header::HeaderValue::from_static("https"),
        );
    }
    // A kept connection the service closed while it was idle: the request
    // goes out once more on a new one where that is safe (`connpool`).
    let Way { key, tls, open, .. } = way;
    let (sender, mut resp) = kept
        .send(&key, client, req.map(BodyExt::boxed), || connect(open, tls))
        .await?;
    let upgraded = resp.status() == StatusCode::SWITCHING_PROTOCOLS && client_upgrade.is_some();
    strip_hop_by_hop(resp.headers_mut(), upgraded);
    if upgraded && let Some(client) = client_upgrade {
        let upstream = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(c), Ok(u)) = (client.await, upstream.await) {
                let (mut c, mut u) = (TokioIo::new(c), TokioIo::new(u));
                let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
            }
        });
        // The upgraded connection is the WebSocket's now: not kept.
        let (parts, _) = resp.into_parts();
        return Ok(hyper::Response::from_parts(
            parts,
            Full::new(Bytes::new()).map_err(|n| match n {}).boxed(),
        ));
    }
    Ok(crate::connpool::GiveBack::wrap(kept, key, sender, resp).map(BodyExt::boxed))
}

/// Remove the hop-by-hop headers (RFC 9110 §7.6.1); an upgrade keeps
/// `Connection` and `Upgrade`.
fn strip_hop_by_hop(h: &mut hyper::HeaderMap, upgrade: bool) {
    let named: Vec<header::HeaderName> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| header::HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for n in named {
        if !(upgrade && n == header::UPGRADE) {
            h.remove(n);
        }
    }
    // The proxy login headers are this hop's too: a visitor's never reaches
    // the service (review before 0.52.0).
    for n in [
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        h.remove(n);
    }
    if !upgrade {
        h.remove(header::CONNECTION);
        h.remove(header::UPGRADE);
    }
}

/// TLS to a service: its certificate is a development one (`insecure_tls`,
/// as its health check takes it), so any is accepted.
fn any_certificate() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("ring supports the default versions")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &[rustls_pki_types::CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

// ------------------------------------------------------- cloudflared

/// `cloudflared`, if not on the `PATH` (a test's fake, an install elsewhere).
const PROGRAM_VARIABLE: &str = "DEVSTACK_CLOUDFLARED";

/// `cloudflared`'s last error line in its output, without its timestamp
/// and level.
pub fn cloudflared_error(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find_map(|l| {
            l.split_once(" ERR ")
                .map(|(_, rest)| rest.trim().to_string())
        })
        .filter(|e| !e.is_empty())
}

/// Where `cloudflared` is: `DEVSTACK_CLOUDFLARED`, else on the `PATH`.
pub fn cloudflared() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(PROGRAM_VARIABLE).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "{PROGRAM_VARIABLE} names {}, which is not a file",
                p.display()
            ))
        };
    }
    find_on_path("cloudflared").ok_or_else(|| {
        "cloudflared is not installed, or not on the PATH of devstack's daemon and hub: \
         `winget install Cloudflare.cloudflared` (Windows), `brew install cloudflared` (macOS), \
         https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/ (Linux); \
         then restart the stack (and the hub, `devstack hub stop`) so they see it"
            .into()
    })
}

/// `name` in the directories of `PATH`, as the shell would find it.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

/// A path as a command line takes it.
pub fn quoted(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_error_line_is_the_reason() {
        let out = "2026-11-03T10:00:00Z INF Starting\n\
                   2026-11-03T10:00:01Z ERR failed to request quick Tunnel: 429 Too Many Requests\n";
        assert_eq!(
            cloudflared_error(out).as_deref(),
            Some("failed to request quick Tunnel: 429 Too Many Requests")
        );
        assert_eq!(cloudflared_error("INF Registered tunnel connection"), None);
    }

    #[tokio::test]
    async fn only_what_the_dial_knows_is_served() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // A service on this machine.
        let svc = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = svc.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = svc.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap();
                    let got = String::from_utf8_lossy(&buf[..n]).to_string();
                    let host = got
                        .lines()
                        .find_map(|l| l.strip_prefix("host: ").or(l.strip_prefix("Host: ")))
                        .unwrap_or("")
                        .to_string();
                    let body = format!("hello {host}");
                    let _ = s
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });
        let router = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = router.local_addr().unwrap();
        let dial: Dial = Arc::new(move |host: String| {
            if host == "web-shop.dev.io" {
                Ok(local(port, false))
            } else {
                Err((404, "no public link is open at this address".into()))
            }
        });
        tokio::spawn(serve(router, dial));
        let get = |host: &'static str| async move {
            let mut s = tokio::net::TcpStream::connect(at).await.unwrap();
            s.write_all(
                format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).await.unwrap();
            out
        };
        let ok = get("web-shop.dev.io").await;
        assert!(
            ok.starts_with("HTTP/1.1 200") && ok.contains(&format!("hello localhost:{port}")),
            "{ok}"
        );
        let refused = get("other.dev.io").await;
        assert!(refused.starts_with("HTTP/1.1 404"), "{refused}");
    }

    /// A service that keeps its connections open and counts them (#559).
    async fn counting_service() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = accepted.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(
                            Bytes::from("pong"),
                        )))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(s), svc)
                        .await;
                });
            }
        });
        (port, accepted)
    }

    /// `GET /` with `host` on a new connection to the router, closed after.
    async fn get_at(at: std::net::SocketAddr, host: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(at).await.unwrap();
        s.write_all(
            format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn the_router_reuses_its_connections_to_a_service() {
        let (port, accepted) = counting_service().await;
        let router = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = router.local_addr().unwrap();
        // The owner's links: which hosts are open now.
        let open = Arc::new(std::sync::Mutex::new(vec!["web-shop.dev.io".to_string()]));
        let links = open.clone();
        let dial: Dial = Arc::new(move |host: String| {
            if links.lock().unwrap().contains(&host) {
                Ok(local(port, false))
            } else {
                Err((404, "no public link is open at this address".into()))
            }
        });
        tokio::spawn(serve(router, dial));
        for _ in 0..20 {
            let r = get_at(at, "web-shop.dev.io").await;
            assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("pong"), "{r}");
        }
        let n = accepted.load(std::sync::atomic::Ordering::SeqCst);
        assert!(n <= 2, "20 requests opened {n} connections to the service");

        // The owner closes the link: refused, although a connection to the
        // service is kept and idle.
        open.lock().unwrap().clear();
        let r = get_at(at, "web-shop.dev.io").await;
        assert!(r.starts_with("HTTP/1.1 404"), "a closed link passed: {r}");
        // That connection was there: open again, it serves without a new one.
        open.lock().unwrap().push("web-shop.dev.io".into());
        let r = get_at(at, "web-shop.dev.io").await;
        assert!(r.ends_with("pong"), "{r}");
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), n);
    }
}
