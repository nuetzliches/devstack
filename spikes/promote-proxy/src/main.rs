//! Spike for ADR-0010 (option B): a daemon-owned L7 reverse proxy on the
//! service's fixed port. It runs the service on internal ports, picks the
//! upstream per request (so a flip takes effect on kept-alive client
//! connections at once), passes streaming bodies (SSE) and upgrades (WebSocket)
//! through, and tags every response with the instance that served it.
//!
//!   promote-proxy --listen 4201 --admin 4299 --ports 14201,14202 --health /health \
//!       --cwd <dir> --env-file <dotenv> -- <command, {port} is replaced>
//!
//! `POST http://127.0.0.1:<admin>/promote` starts a new instance on the other
//! port, waits for health, flips, drains the old one (deadline), stops it
//! gracefully and answers with the timings as JSON.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};

type Body = BoxBody<Bytes, hyper::Error>;

struct Config {
    listen: u16,
    admin: u16,
    ports: [u16; 2],
    health: String,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    command: String,
    drain: Duration,
}

struct State {
    cfg: Config,
    upstream: AtomicU16,
    active: [AtomicUsize; 2],
    instances: Mutex<[Option<devstack_proc::Owned>; 2]>,
}

impl State {
    fn slot(&self, port: u16) -> usize {
        usize::from(port != self.cfg.ports[0])
    }
}

/// Decrements the upstream's active count when the response body (or the
/// upgraded connection) is done.
struct Active(Arc<State>, usize);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active[self.1].fetch_sub(1, Ordering::SeqCst);
    }
}

fn full(text: impl Into<Bytes>) -> Body {
    Full::new(text.into()).map_err(|never| match never {}).boxed()
}

async fn proxy(st: Arc<State>, mut req: Request<Incoming>) -> Result<Response<Body>, hyper::Error> {
    let port = st.upstream.load(Ordering::SeqCst);
    let slot = st.slot(port);
    st.active[slot].fetch_add(1, Ordering::SeqCst);
    let guard = Active(st.clone(), slot);

    let stream = match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(e) => {
            let mut r = Response::new(full(format!("upstream {port}: {e}")));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            return Ok(r);
        }
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(x) => x,
        Err(e) => {
            let mut r = Response::new(full(format!("upstream {port}: {e}")));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            return Ok(r);
        }
    };
    tokio::spawn(conn.with_upgrades());

    let wants_upgrade = req.headers().contains_key(hyper::header::UPGRADE);
    let client_upgrade = wants_upgrade.then(|| hyper::upgrade::on(&mut req));
    let (parts, body) = req.into_parts();
    let upstream_req = Request::from_parts(parts, body);
    let mut resp = match sender.send_request(upstream_req).await {
        Ok(r) => r,
        Err(e) => {
            let mut r = Response::new(full(format!("upstream {port}: {e}")));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            return Ok(r);
        }
    };
    resp.headers_mut()
        .insert("x-devstack-upstream", port.into());

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            let _guard = guard;
            if let (Ok(c), Ok(u)) = (client_upgrade.await, upstream_upgrade.await) {
                let (mut c, mut u) = (TokioIo::new(c), TokioIo::new(u));
                let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
            }
        });
        let (parts, _) = resp.into_parts();
        return Ok(Response::from_parts(parts, full(Bytes::new())));
    }

    // Stream the body through; the guard lives as long as the body does.
    let (parts, body) = resp.into_parts();
    let body = body
        .map_frame(move |f| {
            let _ = &guard;
            f
        })
        .boxed();
    Ok(Response::from_parts(parts, body))
}

async fn healthy(port: u16, path: &str, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        let c = TcpStream::connect(("127.0.0.1", port)).await;
        if std::env::var("SPIKE_DEBUG").is_ok() { eprintln!("connect {port}: {:?}", c.as_ref().map(|_| ()).map_err(|e| e.to_string())); }
        if let Ok(mut s) = c {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            if s.write_all(req.as_bytes()).await.is_ok() {
                let mut buf = Vec::new();
                let _ = tokio::time::timeout(Duration::from_secs(2), s.read_to_end(&mut buf)).await;
                let status = String::from_utf8_lossy(&buf);
                if std::env::var("SPIKE_DEBUG").is_ok() { eprintln!("health {port}: {:?}", status.chars().take(40).collect::<String>()); }
                if status.starts_with("HTTP/1.1 2") || status.starts_with("HTTP/1.0 2") {
                    return true;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn spawn_instance(st: &State, port: u16) -> std::io::Result<devstack_proc::Owned> {
    let mut env = st.cfg.env.clone();
    env.insert("ASPNETCORE_URLS".into(), format!("http://127.0.0.1:{port}"));
    env.insert("DEVSTACK_BUILD_ID".into(), format!("instance-{port}"));
    let log = std::fs::File::create(std::env::temp_dir().join(format!("promote-proxy-{port}.log")))?;
    let spec = devstack_proc::Spec {
        name: format!("svc-{port}"),
        command: st.cfg.command.replace("{port}", &port.to_string()),
        cwd: st.cfg.cwd.clone(),
        env,
        log,
    };
    let opts = devstack_proc::Options {
        cgroup_root: None,
        shim: Vec::new(),
    };
    devstack_proc::spawn(spec, &opts)
        .map(|(o, _)| o)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

async fn promote(st: Arc<State>) -> String {
    let t0 = Instant::now();
    let old = st.upstream.load(Ordering::SeqCst);
    let new = if old == st.cfg.ports[0] { st.cfg.ports[1] } else { st.cfg.ports[0] };
    let tree = match spawn_instance(&st, new) {
        Ok(t) => t,
        Err(e) => return format!("{{\"error\":\"spawn: {e}\"}}"),
    };
    st.instances.lock().unwrap()[st.slot(new)] = Some(tree);
    if !healthy(new, &st.cfg.health, Duration::from_secs(180)).await {
        return "{\"error\":\"new instance not healthy\"}".into();
    }
    let ready = t0.elapsed();
    st.upstream.store(new, Ordering::SeqCst);
    let flipped = t0.elapsed();
    let deadline = Instant::now() + st.cfg.drain;
    while st.active[st.slot(old)].load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let left = st.active[st.slot(old)].load(Ordering::SeqCst);
    let drained = t0.elapsed();
    let old_tree = st.instances.lock().unwrap()[st.slot(old)].take();
    let graceful = match old_tree {
        Some(mut t) => tokio::task::spawn_blocking(move || t.stop(Duration::from_secs(10)).graceful)
            .await
            .unwrap_or(false),
        None => true,
    };
    let stopped = t0.elapsed();
    format!(
        "{{\"from\":{old},\"to\":{new},\"ready_ms\":{},\"flip_ms\":{},\"drained_ms\":{},\"stopped_ms\":{},\"left_at_deadline\":{left},\"graceful\":{graceful}}}",
        ready.as_millis(),
        flipped.as_millis(),
        drained.as_millis(),
        stopped.as_millis()
    )
}

async fn admin(st: Arc<State>, req: Request<Incoming>) -> Result<Response<Body>, hyper::Error> {
    let body = match req.uri().path() {
        "/promote" => promote(st).await,
        "/upstream" => st.upstream.load(Ordering::SeqCst).to_string(),
        _ => "unknown".into(),
    };
    Ok(Response::new(full(body)))
}

fn parse() -> Config {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1).cloned())
            .unwrap_or_else(|| panic!("missing {k}"))
    };
    let ports: Vec<u16> = get("--ports").split(',').map(|p| p.parse().unwrap()).collect();
    let mut env = BTreeMap::new();
    if let Some(i) = args.iter().position(|a| a == "--env-file") {
        let text = std::fs::read_to_string(&args[i + 1]).expect("env file");
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .map(|v| v.replace("\\\"", "\"").replace("\\$", "$").replace("\\\\", "\\"))
                    .unwrap_or_else(|| v.to_string());
                env.insert(k.trim().to_string(), v);
            }
        }
    }
    let command = args
        .iter()
        .position(|a| a == "--")
        .map(|i| args[i + 1..].join(" "))
        .expect("command after --");
    Config {
        listen: get("--listen").parse().unwrap(),
        admin: get("--admin").parse().unwrap(),
        ports: [ports[0], ports[1]],
        health: get("--health"),
        cwd: PathBuf::from(get("--cwd")),
        env,
        command,
        drain: Duration::from_millis(
            args.iter()
                .position(|a| a == "--drain-ms")
                .map(|i| args[i + 1].parse().unwrap())
                .unwrap_or(5000),
        ),
    }
}

#[tokio::main]
async fn main() {
    devstack_proc::ensure_console().expect("console");
    let cfg = parse();
    let st = Arc::new(State {
        upstream: AtomicU16::new(cfg.ports[0]),
        active: [AtomicUsize::new(0), AtomicUsize::new(0)],
        instances: Mutex::new([None, None]),
        cfg,
    });
    let first = spawn_instance(&st, st.cfg.ports[0]).expect("first instance");
    st.instances.lock().unwrap()[0] = Some(first);
    assert!(
        healthy(st.cfg.ports[0], &st.cfg.health, Duration::from_secs(180)).await,
        "first instance not healthy"
    );
    println!("ready: :{} -> :{}", st.cfg.listen, st.cfg.ports[0]);

    let a = st.clone();
    let admin_listener = TcpListener::bind(("127.0.0.1", st.cfg.admin)).await.unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = admin_listener.accept().await else { continue };
            let a = a.clone();
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(s), service_fn(move |r| admin(a.clone(), r)))
                    .await;
            });
        }
    });

    let listener = TcpListener::bind(("127.0.0.1", st.cfg.listen)).await.unwrap();
    loop {
        let Ok((s, _)) = listener.accept().await else { continue };
        let st = st.clone();
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(s), service_fn(move |r| proxy(st.clone(), r)))
                .with_upgrades()
                .await;
        });
    }
}
