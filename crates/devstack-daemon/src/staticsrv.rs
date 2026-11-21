//! The server of a `kind = "static"` service (plan §3.4): the production
//! bundle of a frontend, built into an artifact, served as files. It runs as
//! the service's process (`devstack serve-static <dir> --port <p>`), so the
//! candidate pipeline, the proxy and the health check treat it like any
//! service, and an end-to-end run pins a build id.
//!
//! Its `forward`s (ADR-0110) send requests below a path prefix to another
//! server, as a reverse proxy in front of the bundle would in production:
//! a single-page app that calls its API at a relative `/api` finds it on its
//! own origin.

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, Response, StatusCode, Uri, Version, header};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// The file a request path names below `root`, never outside it.
pub fn resolve(root: &Path, uri_path: &str, spa: bool) -> Option<PathBuf> {
    let decoded = percent_decode(uri_path.split('?').next().unwrap_or(""));
    let mut p = root.to_path_buf();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(n) => p.push(n),
            Component::CurDir => {}
            _ => return None, // `..`, a drive, a root: not below `root`
        }
    }
    if p.is_dir() {
        p.push("index.html");
    }
    if p.is_file() {
        return Some(p);
    }
    // A client-side route (no file extension): the app's index.html.
    let last = decoded.rsplit('/').next().unwrap_or("");
    (spa && !last.contains('.'))
        .then(|| root.join("index.html"))
        .filter(|i| i.is_file())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

use devstack_node::connpool::{Bound, GiveBack, IDLE_MAX, Pool};

/// One client connection: where it comes from, and the connection to each
/// forward's target a login bound to it (#569). axum makes one per
/// connection it accepts and hands a copy to each of its requests.
#[derive(Clone)]
struct Client {
    peer: SocketAddr,
    bound: Arc<std::sync::Mutex<Bindings>>,
}

/// A client connection's bindings, by the port of the forward's target.
type Bindings = std::collections::HashMap<u16, Arc<Bound<(), Body>>>;

impl Client {
    /// Its binding to the target on `port` (one per forward).
    fn to(&self, port: u16) -> Arc<Bound<(), Body>> {
        self.bound.lock().unwrap().entry(port).or_default().clone()
    }
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for Client
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Client {
            peer: *stream.remote_addr(),
            bound: Arc::default(),
        }
    }
}

/// Where a forward sends its requests: `http://host:port`, with the
/// connections to it that are idle now. They are reused, as a reverse proxy
/// does: a connection per request would leave each one in `TIME_WAIT` on
/// this side, and an end-to-end suite drains the ephemeral ports (#556).
#[derive(Debug, Clone)]
pub struct Target {
    host: String,
    port: u16,
    pool: Arc<Pool<(), Body>>,
}

impl Target {
    /// A new connection.
    async fn connect(&self) -> Result<hyper::client::conn::http1::SendRequest<Body>, String> {
        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| e.to_string())?;
        let _ = stream.set_nodelay(true);
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn.with_upgrades());
        Ok(sender)
    }
}

/// The `--forward PREFIX=URL` arguments, checked as `devstack check` does.
pub fn forwards(args: &[String]) -> Result<Vec<(String, Target)>, String> {
    use devstack_config::forward;
    args.iter()
        .map(|a| {
            let (prefix, url) = a
                .split_once('=')
                .ok_or_else(|| format!("--forward `{a}`: takes PREFIX=URL"))?;
            forward::prefix(prefix).map_err(|e| format!("--forward `{prefix}`: {e}"))?;
            let (host, port) =
                forward::origin(url).map_err(|e| format!("--forward {prefix}: `{url}`: {e}"))?;
            Ok((
                prefix.to_string(),
                Target {
                    host,
                    port,
                    pool: Pool::new(IDLE_MAX),
                },
            ))
        })
        .collect()
}

/// Serve `root` on 127.0.0.1:`port` until the process ends; requests below
/// a prefix of `forward` (`PREFIX=URL`) go to its target.
pub fn serve(root: PathBuf, port: u16, spa: bool, forward: &[String]) -> Result<(), String> {
    if !root.join("index.html").is_file() && spa {
        return Err(format!("{} has no index.html", root.display()));
    }
    let forwards = forwards(forward)?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("port {port}: {e}"))?;
        axum::serve(
            listener,
            app(root, spa, forwards).into_make_service_with_connect_info::<Client>(),
        )
        .await
        .map_err(|e| e.to_string())
    })
}

fn app(root: PathBuf, spa: bool, forwards: Vec<(String, Target)>) -> axum::Router {
    let forwards = Arc::new(forwards);
    axum::Router::new().fallback(
        move |ConnectInfo(client): ConnectInfo<Client>, req: Request| {
            let root = root.clone();
            let forwards = forwards.clone();
            async move {
                // A forwarded prefix never falls back to a file.
                if let Some((_, to)) = devstack_config::forward::pick(&forwards, req.uri().path()) {
                    return pass(to, &client, req).await;
                }
                let tag = req
                    .headers()
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                file(&root, req.uri().path(), spa, tag.as_deref()).await
            }
        },
    )
}

/// Send `req` on to `to`, as a reverse proxy does: the path unchanged,
/// `Host` as the browser sent it, `X-Forwarded-*` saying where it came from
/// (kept where a proxy in front, a tunnel, set them), hop-by-hop headers on
/// their hop, the answer streamed (SSE, a download) and an upgrade
/// (WebSocket) joined. An unreachable target is a 502 at once: nothing here
/// knows whether it is starting.
async fn pass(to: &Target, client: &Client, mut req: Request) -> Response<Body> {
    let peer = client.peer;
    let fail = |e: String| {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from(format!(
                "devstack: forward to http://{}:{}: {e}\n",
                to.host, to.port
            )))
            .unwrap()
    };
    let client_upgrade = req
        .headers()
        .contains_key(header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    let host = req
        .headers()
        .get(header::HOST)
        .cloned()
        .or_else(|| req.uri().authority().and_then(|a| a.as_str().parse().ok()));
    let h = req.headers_mut();
    crate::proxy::strip_hop_by_hop(h, client_upgrade.is_some());
    let from = match h.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        Some(before) => format!("{before}, {}", peer.ip()),
        None => peer.ip().to_string(),
    };
    if let Ok(v) = from.parse() {
        h.insert("x-forwarded-for", v);
    }
    if !h.contains_key("x-forwarded-proto") {
        h.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    }
    if let Some(host) = host {
        if !h.contains_key("x-forwarded-host") {
            h.insert("x-forwarded-host", host.clone());
        }
        h.insert(header::HOST, host);
    }
    // HTTP/1.1 to the target, the request target in origin form.
    let path = req
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_string();
    *req.uri_mut() = path.parse().unwrap_or_else(|_| Uri::from_static("/"));
    *req.version_mut() = Version::HTTP_11;

    let bound = client.to(to.port);
    let (sender, mut resp) = match to.pool.send(&(), &bound, req, || to.connect()).await {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let upgraded = resp.status() == StatusCode::SWITCHING_PROTOCOLS && client_upgrade.is_some();
    crate::proxy::strip_hop_by_hop(resp.headers_mut(), upgraded);
    if upgraded && let Some(client_upgrade) = client_upgrade {
        let target_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(c), Ok(t)) = (client_upgrade.await, target_upgrade.await) {
                let (mut c, mut t) = (TokioIo::new(c), TokioIo::new(t));
                let _ = tokio::io::copy_bidirectional(&mut c, &mut t).await;
            }
        });
        // The upgraded connection is the WebSocket's now: not given back.
        let (parts, _) = resp.into_parts();
        return Response::from_parts(parts, Body::empty());
    }
    // Read to its end, the answer's connection takes the next request. One
    // the browser leaves early (or a stream that does not end, SSE) keeps or
    // drops its connection, never back in the pool.
    GiveBack::wrap(to.pool.clone(), (), sender, resp).map(Body::new)
}

/// Whether a file's name carries a hash of its content (`main-3f2a1b9c.js`,
/// `index.Bd3x9_kQ.css`), so a changed file has a new name: the last part of
/// its stem, after `-` or `.`, 8 to 32 letters, digits or `_`, with a digit.
fn content_hashed(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some((_, last)) = stem.rsplit_once(['-', '.']) else {
        return false;
    };
    (8..=32).contains(&last.len())
        && last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && last.chars().any(|c| c.is_ascii_digit())
}

/// How a file may be cached: one whose name has its content hash for a year;
/// any other (`index.html`, `config.json`, a manifest) asked again each time,
/// answered `304` while its `ETag` holds, so a new build shows at once.
fn cache_control(path: &Path) -> &'static str {
    if content_hashed(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Whether `If-None-Match` names `etag` (or `*`).
fn matches_etag(if_none_match: &str, etag: &str) -> bool {
    if_none_match
        .split(',')
        .map(|t| t.trim().trim_start_matches("W/"))
        .any(|t| t == etag || t == "*")
}

async fn file(root: &Path, path: &str, spa: bool, if_none_match: Option<&str>) -> Response<Body> {
    let Some(p) = resolve(root, path, spa) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap();
    };
    let read = {
        let p = p.clone();
        tokio::task::spawn_blocking(move || std::fs::read(p)).await
    };
    match read.unwrap_or_else(|e| Err(std::io::Error::other(e))) {
        Ok(bytes) => {
            let etag = format!("\"{}\"", &blake3::hash(&bytes).to_hex()[..32]);
            let fresh = if_none_match.is_some_and(|t| matches_etag(t, &etag));
            let mut r = if fresh {
                let mut r = Response::new(Body::empty());
                *r.status_mut() = StatusCode::NOT_MODIFIED;
                r
            } else {
                Response::new(Body::from(bytes))
            };
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&p)),
            );
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control(&p)),
            );
            if let Ok(v) = HeaderValue::from_str(&etag) {
                h.insert(header::ETAG, v);
            }
            r
        }
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("cannot read the file"))
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_routes_and_no_escape() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::write(r.join("index.html"), "<app>").unwrap();
        std::fs::create_dir_all(r.join("assets")).unwrap();
        std::fs::write(r.join("assets/main-ab12.js"), "x").unwrap();
        assert_eq!(resolve(r, "/", true), Some(r.join("index.html")));
        assert_eq!(
            resolve(r, "/assets/main-ab12.js", true),
            Some(r.join("assets/main-ab12.js"))
        );
        assert_eq!(
            resolve(r, "/customers/42", true),
            Some(r.join("index.html")),
            "a client route"
        );
        assert_eq!(resolve(r, "/customers/42", false), None, "no SPA fallback");
        assert_eq!(
            resolve(r, "/assets/missing.js", true),
            None,
            "a missing file is a 404"
        );
        assert_eq!(resolve(r, "/../secret.txt", true), None);
        assert_eq!(resolve(r, "/%2e%2e/secret.txt", true), None);
        assert_eq!(content_type(Path::new("a.woff2")), "font/woff2");
    }

    #[test]
    fn only_content_hashed_names_are_cached_for_long() {
        for hashed in [
            "assets/index-Bd3x9_kQ.js",
            "main.3f2a1b9c.js",
            "chunk-1a2b3c4d5e6f.css",
        ] {
            assert_eq!(
                cache_control(Path::new(hashed)),
                "public, max-age=31536000, immutable",
                "{hashed}"
            );
        }
        for plain in [
            "index.html",
            "assets/config.json",
            "manifest.webmanifest",
            "i18n/de.json",
            "polyfills.js",
            "chunk-vendors.js",
            "favicon.ico",
        ] {
            assert_eq!(cache_control(Path::new(plain)), "no-cache", "{plain}");
        }
        assert!(matches_etag("\"a\", W/\"b\"", "\"b\""));
        assert!(matches_etag("*", "\"b\""));
        assert!(!matches_etag("\"a\"", "\"b\""));
    }

    /// The API behind a forward: echoes what it got, streams an event that
    /// never ends, and echoes on a WebSocket.
    async fn api() -> u16 {
        use axum::extract::ws::{Message, WebSocketUpgrade};
        use axum::routing::{any, get};
        let echo = |req: Request| async move {
            let h = |n: &str| {
                req.headers()
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string()
            };
            format!(
                "{} {} host={} xff={} xfh={} xfp={}",
                req.method(),
                req.uri(),
                h("host"),
                h("x-forwarded-for"),
                h("x-forwarded-host"),
                h("x-forwarded-proto")
            )
        };
        let sse = || async {
            use tokio_stream::StreamExt;
            let first =
                tokio_stream::once(Ok::<_, std::io::Error>(bytes::Bytes::from("data: one\n\n")));
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(first.chain(tokio_stream::pending())))
                .unwrap()
        };
        let ws = |up: WebSocketUpgrade| async {
            up.on_upgrade(|mut s| async move {
                while let Some(Ok(Message::Text(t))) = s.recv().await {
                    let _ = s.send(Message::Text(t)).await;
                }
            })
        };
        let app = axum::Router::new()
            .route("/api/sse", get(sse))
            .route("/api/ws", get(ws))
            .fallback(any(echo));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(l, app).await });
        port
    }

    async fn site(root: &Path, forward: &[String]) -> u16 {
        let app = app(root.to_path_buf(), true, forwards(forward).unwrap());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, app.into_make_service_with_connect_info::<Client>()).await
        });
        port
    }

    /// Send `raw` and read until `until` shows up (or the connection ends).
    async fn talk(port: u16, raw: &[u8], until: &str) -> (tokio::net::TcpStream, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        c.write_all(raw).await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let read = async {
            while !String::from_utf8_lossy(&got).contains(until) {
                match c.read(&mut buf).await.unwrap() {
                    0 => break,
                    n => got.extend_from_slice(&buf[..n]),
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), read)
            .await
            .expect("an answer in time");
        let text = String::from_utf8_lossy(&got).into_owned();
        (c, text)
    }

    #[tokio::test]
    async fn a_forward_reaches_the_api_on_the_sites_origin() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("index.html"), "<app>").unwrap();
        let api = api().await;
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let port = site(
            d.path(),
            &[
                format!("/api=http://127.0.0.1:{api}"),
                format!("/gone=http://127.0.0.1:{closed}/"),
            ],
        )
        .await;
        let get = |path: &str| {
            format!("GET {path} HTTP/1.1\r\nHost: shop.test:{port}\r\nConnection: close\r\n\r\n")
        };

        // The path and query go on unchanged, Host as sent, X-Forwarded-*.
        let (_, r) = talk(port, get("/api/users/7?q=a%20b").as_bytes(), "\0").await;
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(
            r.contains(&format!(
                "GET /api/users/7?q=a%20b host=shop.test:{port} xff=127.0.0.1 xfh=shop.test:{port} xfp=http"
            )),
            "{r}"
        );
        // Not at a segment boundary, or not below a prefix: the app.
        for path in ["/apix", "/customers/42", "/"] {
            let (_, r) = talk(port, get(path).as_bytes(), "\0").await;
            assert!(r.contains("<app>"), "{path}: {r}");
        }
        // A server-sent event arrives while its stream is still open.
        let (_, r) = talk(port, get("/api/sse").as_bytes(), "data: one").await;
        assert!(
            r.contains("text/event-stream") && r.contains("data: one"),
            "{r}"
        );
        // A WebSocket is joined through.
        let upgrade = format!(
            "GET /api/ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        let (mut c, r) = talk(port, upgrade.as_bytes(), "\r\n\r\n").await;
        assert!(r.starts_with("HTTP/1.1 101"), "{r}");
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mask = [1u8, 2, 3, 4];
            let mut frame = vec![0x81, 0x80 | 2];
            frame.extend_from_slice(&mask);
            frame.extend(b"hi".iter().zip(mask).map(|(b, m)| b ^ m));
            c.write_all(&frame).await.unwrap();
            let mut back = [0u8; 4];
            tokio::time::timeout(std::time::Duration::from_secs(10), c.read_exact(&mut back))
                .await
                .expect("the echo in time")
                .unwrap();
            assert_eq!(back, [0x81, 2, b'h', b'i']);
        }
        // A target that does not answer: 502 at once, naming it.
        let (_, r) = talk(port, get("/gone/x").as_bytes(), "\0").await;
        assert!(
            r.starts_with("HTTP/1.1 502") && r.contains(&format!("127.0.0.1:{closed}")),
            "{r}"
        );
    }

    /// A target that counts the connections it accepts (#556).
    async fn counting_target() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = accepted.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(Response::new(http_body_util::Full::new(
                            bytes::Bytes::from("pong"),
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

    #[tokio::test]
    async fn a_forward_reuses_its_connections_to_the_target() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("index.html"), "<app>").unwrap();
        let (target, accepted) = counting_target().await;
        let port = site(d.path(), &[format!("/api=http://127.0.0.1:{target}")]).await;
        // One browser connection per request, closed after it, as a test
        // suite's many pages do: the target's side must not follow suit.
        for i in 0..30 {
            let (_, r) = talk(
                port,
                format!("GET /api/ping/{i} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
                "\0",
            )
            .await;
            assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("pong"), "{r}");
        }
        let n = accepted.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            n <= 2,
            "30 requests one after another opened {n} connections to the target"
        );
        // Four at a time, as four test workers send them: about four.
        for round in 0..10 {
            let all = (0..4).map(|w| {
                talk(
                    port,
                    format!(
                        "GET /api/{round}/{w} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
                    )
                    .leak()
                    .as_bytes(),
                    "\0",
                )
            });
            for (_, r) in futures_join(all).await {
                assert!(r.ends_with("pong"), "{r}");
            }
        }
        let n = accepted.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            n <= 8,
            "70 requests, four at a time, opened {n} connections to the target"
        );
    }

    /// Run the futures side by side, their results in order.
    async fn futures_join<F: std::future::Future + Send + 'static>(
        fs: impl Iterator<Item = F>,
    ) -> Vec<F::Output>
    where
        F::Output: Send + 'static,
    {
        let handles: Vec<_> = fs.map(tokio::spawn).collect();
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    }

    #[test]
    fn a_forward_argument_is_checked_at_the_start() {
        assert!(forwards(&["/api=http://127.0.0.1:5000".into()]).is_ok());
        for bad in [
            "/api",
            "api=http://127.0.0.1:5000",
            "/api=https://127.0.0.1:5001",
            "/api=http://127.0.0.1:5000/v1",
        ] {
            assert!(forwards(&[bad.into()]).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn an_unchanged_file_is_not_sent_again() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.json"), "{}").unwrap();
        let r = file(d.path(), "/config.json", false, None).await;
        assert_eq!(r.status(), StatusCode::OK);
        let etag = r.headers()[header::ETAG].to_str().unwrap().to_string();
        let r = file(d.path(), "/config.json", false, Some(&etag)).await;
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
        std::fs::write(d.path().join("config.json"), "{\"a\":1}").unwrap();
        let r = file(d.path(), "/config.json", false, Some(&etag)).await;
        assert_eq!(r.status(), StatusCode::OK, "changed: sent again");
    }
}
