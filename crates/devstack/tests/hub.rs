//! The hub as a black box (ADR-0021, ADR-0019): pairing, the session cookie,
//! Host and Origin checks, the stack stream, the app shell, forwarding to a
//! stack that is not running. Runs on a free port with its own profile
//! directory, so it never touches the user's hub.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_devstack");

struct Hub {
    child: Child,
    port: u16,
    home: tempfile::TempDir,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start() -> Hub {
    start_in(tempfile::tempdir().unwrap(), free_port())
}

fn start_in(home: tempfile::TempDir, port: u16) -> Hub {
    let child = Command::new(BIN)
        .arg("hub")
        .env("DEVSTACK_HUB_PORT", port.to_string())
        .env("LOCALAPPDATA", home.path())
        .env("XDG_STATE_HOME", home.path())
        .env("DEVSTACK_NO_UPDATE_CHECK", "1")
        // What would notify is logged; nothing shows on the desktop.
        .env("DEVSTACK_NO_NOTIFY", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(15), "hub did not start");
        std::thread::sleep(Duration::from_millis(100));
    }
    Hub { child, port, home }
}

/// A raw HTTP/1.1 request; returns (status, headers lowercased, body).
fn http(port: u16, req: &str) -> (u16, String, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, head.to_ascii_lowercase(), body.to_string())
}

fn get(port: u16, path: &str, extra: &str) -> (u16, String, String) {
    http(
        port,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{extra}\r\n"
        ),
    )
}

fn token(h: &Hub) -> String {
    std::fs::read_to_string(h.home.path().join("devstack/token"))
        .unwrap()
        .trim()
        .to_string()
}

fn pair(h: &Hub) -> String {
    let (status, _, body) = http(
        h.port,
        &format!(
            "POST /hub/pair HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            h.port,
            token(h)
        ),
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    v["code"].as_str().unwrap().to_string()
}

#[test]
fn pairing_cookie_and_checks() {
    let h = start();
    let p = h.port;

    // The app shell needs no session; client-side routes get index.html.
    let (s, head, body) = get(p, "/", "");
    assert_eq!(s, 200);
    assert!(head.contains("text/html") && body.contains("<"), "{head}");
    // H-2: never in a frame, no inline script, no sniffing, no referrer.
    for h in [
        "content-security-policy: default-src 'self'; script-src 'self';",
        "frame-ancestors 'none'",
        "x-frame-options: deny",
        "x-content-type-options: nosniff",
        "referrer-policy: no-referrer",
    ] {
        assert!(head.contains(h), "{h} missing: {head}");
    }
    // The dashboard itself has no inline script the policy would block.
    for tag in body.split("<script").skip(1) {
        let open = tag.split('>').next().unwrap_or("");
        assert!(open.contains("src="), "inline script in index.html: {body}");
    }
    let (s, _, _) = get(p, "/stacks/abc/services/api", "");
    assert_eq!(s, 200, "SPA fallback");
    // With the real dashboard embedded (CI builds ui/dist first): every
    // asset the shell names is served, cached for good; the shell is not.
    assert!(head.to_lowercase().contains("no-cache"), "{head}");
    for asset in body
        .split(['"', '\''])
        .filter(|s| s.starts_with("/assets/"))
    {
        let (s, h, _) = get(p, asset, "");
        assert_eq!(s, 200, "{asset}");
        assert!(h.to_lowercase().contains("immutable"), "{asset}: {h}");
    }

    // Not paired.
    let (s, _, body) = get(p, "/hub/session", "");
    assert_eq!(s, 401);
    assert!(body.contains("devstack ui"), "{body}");

    // A foreign Host (DNS rebinding) is refused before anything else.
    let (s, _, _) = http(
        p,
        "GET / HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(s, 403);

    // Pairing: a one-time code becomes a cookie, once.
    let code = pair(&h);
    let (s, head, _) = get(p, &format!("/auth?code={code}"), "");
    assert_eq!(s, 303, "{head}");
    let cookie = head
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .expect("a cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(
        head.contains("httponly") && head.contains("samesite=strict"),
        "{head}"
    );
    let (s, _, _) = get(p, &format!("/auth?code={code}"), "");
    assert_eq!(s, 401, "a code works once");

    // H-1: the cookie alone is not enough, a page on another loopback port
    // sends it too (same-site); the request must come from the hub's page.
    let (s, _, _) = get(p, "/hub/session", &format!("Cookie: {cookie}\r\n"));
    assert_eq!(s, 403, "no Sec-Fetch-Site, no Origin");
    let (s, _, _) = get(
        p,
        "/hub/session",
        &format!("Cookie: {cookie}\r\nSec-Fetch-Site: same-site\r\n"),
    );
    assert_eq!(s, 403, "another port of 127.0.0.1");
    let (s, _, _) = get(
        p,
        "/hub/session",
        &format!("Cookie: {cookie}\r\nOrigin: http://127.0.0.1:{p}\r\n"),
    );
    assert_eq!(s, 200, "own Origin");

    let with = format!("Cookie: {cookie}\r\nSec-Fetch-Site: same-origin\r\n");
    let (s, _, body) = get(p, "/hub/session", &with);
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("version"));
    // Another server on a loopback port may set a cookie of the same name;
    // the browser then sends both, and the real one still counts (H-9).
    let (s, _, _) = get(
        p,
        "/hub/session",
        &format!("Cookie: devstack_hub=junk; {cookie}\r\nSec-Fetch-Site: same-origin\r\n"),
    );
    assert_eq!(s, 200, "a second cookie of the same name");

    // Mutating with the cookie needs the hub's own Origin.
    let post = |origin: &str| {
        http(
            p,
            &format!(
                "POST /hub/stacks/nope/up HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}{origin}Content-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
        .0
    };
    assert_eq!(post(""), 403, "no Origin");
    assert_eq!(
        post("Origin: https://evil.example\r\n"),
        403,
        "foreign Origin"
    );
    assert_eq!(
        post(&format!("Origin: http://127.0.0.1:{p}\r\n")),
        404,
        "own Origin passes (no such stack)"
    );

    // A registered stack that does not run: listed as not running, forwarding says so.
    let reg = h.home.path().join("devstack/stacks");
    std::fs::create_dir_all(&reg).unwrap();
    let repo = h.home.path().join("shop");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        reg.join("0123456789ab.json"),
        serde_json::json!({
            "id": "0123456789ab", "name": "shop",
            "file": repo.join("devstack.toml"), "repo": repo
        })
        .to_string(),
    )
    .unwrap();
    let t0 = Instant::now();
    loop {
        let (_, _, body) = get(p, "/hub/stacks", &with);
        if body.contains("0123456789ab") {
            assert!(body.contains("\"running\":false"), "{body}");
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "stack not listed: {body}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let (s, _, body) = get(p, "/hub/stacks/0123456789ab/api/v1/stack", &with);
    assert_eq!(s, 503, "{body}");
    assert!(body.contains("not running"));

    // The stack stream starts with the list.
    let mut st = std::net::TcpStream::connect(("127.0.0.1", p)).unwrap();
    st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        st,
        "GET /hub/events HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}\r\n"
    )
    .unwrap();
    let mut r = BufReader::new(st);
    let mut line = String::new();
    let t0 = Instant::now();
    while !line.starts_with("data:") {
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(t0.elapsed() < Duration::from_secs(5));
    }
    assert!(
        line.contains("\"type\":\"stacks\"") && line.contains("shop"),
        "{line}"
    );
}

/// The cookie from a fresh pairing (`name=value`).
fn paired_cookie(h: &Hub) -> String {
    let code = pair(h);
    let (_, head, _) = get(h.port, &format!("/auth?code={code}"), "");
    head.lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .expect("a cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn stops_with_an_open_stream_and_keeps_the_pairing() {
    let mut h = start();
    let p = h.port;
    let with = format!(
        "Cookie: {}\r\nSec-Fetch-Site: same-origin\r\n",
        paired_cookie(&h)
    );

    // A dashboard tab holds the stack stream open.
    let mut st = std::net::TcpStream::connect(("127.0.0.1", p)).unwrap();
    write!(
        st,
        "GET /hub/events HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}\r\n"
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let (s, _, _) = http(
        p,
        &format!(
            "POST /hub/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            token(&h)
        ),
    );
    assert_eq!(s, 202);
    let t0 = Instant::now();
    while h.child.try_wait().unwrap().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the open stream kept the hub alive"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(st);

    // Same profile, new process: the browser is still paired.
    let home = std::mem::replace(&mut h.home, tempfile::tempdir().unwrap());
    let h2 = start_in(home, p);
    let (s, _, body) = get(p, "/hub/session", &with);
    assert_eq!(s, 200, "{body}");
    let (s, _, _) = get(p, "/hub/session", "Cookie: devstack_hub=forged\r\n");
    assert_eq!(s, 401);
    let sessions = h2.home.path().join("devstack/hub/hub-sessions.json");
    let stored = std::fs::read_to_string(&sessions).unwrap();
    let value = with
        .trim_start_matches("Cookie: devstack_hub=")
        .split("\r\n")
        .next()
        .unwrap();
    assert!(!stored.contains(value), "cookie stored in clear");

    // Deleting the file unpairs every browser at once, not at the next start.
    std::fs::remove_file(&sessions).unwrap();
    let (s, _, _) = get(p, "/hub/session", &with);
    assert_eq!(s, 401, "still paired after the file was deleted");
}

#[test]
fn ping_is_public_but_tells_only_the_landing_page() {
    let h = start();
    let p = h.port;
    // No session needed, and nothing but the version in it.
    let (s, head, body) = get(p, "/hub/ping", "Origin: https://nuetzliches.github.io\r\n");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    assert_eq!(v["devstack"], true);
    assert!(v["version"].is_string());
    assert_eq!(v.as_object().unwrap().len(), 2, "{body}");
    assert!(
        head.contains("access-control-allow-origin: https://nuetzliches.github.io"),
        "{head}"
    );
    assert!(
        head.contains("access-control-allow-private-network: true"),
        "{head}"
    );

    // Another page gets no CORS header, so its script cannot read the answer.
    let (s, head, _) = get(p, "/hub/ping", "Origin: https://evil.example\r\n");
    assert_eq!(s, 200);
    assert!(!head.contains("access-control-allow-origin"), "{head}");

    // The preflight Chrome sends before a public page may reach this machine.
    let (s, head, _) = http(
        p,
        &format!(
            "OPTIONS /hub/ping HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: https://nuetzliches.github.io\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Private-Network: true\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(s, 204, "{head}");
    assert!(head.contains("access-control-allow-methods: get"), "{head}");

    // The rest of /hub stays closed.
    let (s, _, _) = get(
        p,
        "/hub/stacks",
        "Origin: https://nuetzliches.github.io\r\n",
    );
    assert_eq!(s, 401);

    // With a nonce, a proof that the hub knows the token (never the token).
    let (s, _, body) = get(p, "/hub/ping?nonce=0123456789abcdef0123", "");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let proof = v["proof"].as_str().expect("a proof");
    assert_eq!(proof.len(), 64);
    assert!(!body.contains(&token(&h)));
}

/// H-3: the CLI sends the token only to a hub that proves it knows it. A
/// program of another user on the hub's port gets nothing.
#[test]
fn the_token_goes_only_to_this_users_hub() {
    let h = start();
    let cli = |port: u16, args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .env("DEVSTACK_HUB_PORT", port.to_string())
            .env("LOCALAPPDATA", h.home.path())
            .env("XDG_STATE_HOME", h.home.path())
            .env("DEVSTACK_NO_UPDATE_CHECK", "1")
            .output()
            .unwrap()
    };
    let out = cli(h.port, &["hub", "status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("hub running"), "{text}");

    // A squatter that looks like a hub, and records what it is sent.
    let squat = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = squat.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    {
        let seen = seen.clone();
        std::thread::spawn(move || {
            for c in squat.incoming().flatten() {
                let mut c = c;
                c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).unwrap_or(0);
                seen.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
                let body = r#"{"devstack":true,"version":"0.9.8","proof":"00"}"#;
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
    }
    let out = cli(port, &["hub", "stop"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not as your devstack hub"), "{err}");
    let out = cli(port, &["ui", "--print"]);
    assert!(!out.status.success());
    let seen = seen.lock().unwrap();
    assert!(seen.contains("/hub/ping?nonce="), "{seen}");
    assert!(!seen.contains(&token(&h)), "the token went to the squatter");
    assert!(
        !seen.to_ascii_lowercase().contains("authorization"),
        "{seen}"
    );
}

#[test]
fn a_stack_is_added_or_drafted_from_the_dashboard() {
    let h = start();
    let p = h.port;
    let with = format!(
        "Cookie: {}\r\nSec-Fetch-Site: same-origin\r\n",
        paired_cookie(&h)
    );
    let post = |path: &str, body: &str, origin: bool| {
        let o = if origin {
            format!("Origin: http://127.0.0.1:{p}\r\n")
        } else {
            String::new()
        };
        http(
            p,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}{o}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let js = |s: &str| serde_json::to_string(s).unwrap();

    // A folder with a stack file: inspect, add, listed.
    let shop = h.home.path().join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    std::fs::write(
        shop.join("devstack.toml"),
        "version = 1\n[stack]\nname = \"shop\"\n",
    )
    .unwrap();
    let dir = shop.to_string_lossy().to_string();
    let (s, _, body) = get(p, &format!("/hub/inspect?path={}", enc(&dir)), &with);
    assert_eq!(s, 200, "{body}");
    assert!(
        body.contains("\"has_stack_file\":true") && body.contains("\"registered\":null"),
        "{body}"
    );
    let (s, _, _) = post(
        "/hub/stacks/add",
        &format!("{{\"path\":{}}}", js(&dir)),
        false,
    );
    assert_eq!(s, 403, "no Origin, no change");
    let (s, _, body) = post(
        "/hub/stacks/add",
        &format!("{{\"path\":{}}}", js(&dir)),
        true,
    );
    assert_eq!(s, 200, "{body}");
    // The hub rescans its registry once a second.
    let t0 = Instant::now();
    loop {
        let (_, _, list) = get(p, "/hub/stacks", &with);
        if list.contains("\"name\":\"shop\"") {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "not listed: {list}");
        std::thread::sleep(Duration::from_millis(200));
    }

    // An empty folder: a draft is written (never over an existing file).
    let crm = h.home.path().join("crm");
    std::fs::create_dir_all(&crm).unwrap();
    let (s, _, body) = post(
        "/hub/stacks/init",
        &format!("{{\"path\":{}}}", js(&crm.to_string_lossy())),
        true,
    );
    assert_eq!(s, 200, "{body}");
    assert!(crm.join("devstack.toml").is_file());
    let (s, _, _) = post(
        "/hub/stacks/init",
        &format!("{{\"path\":{}}}", js(&crm.to_string_lossy())),
        true,
    );
    assert_eq!(s, 409, "an existing stack file stays");

    // Relative paths are refused.
    let (s, _, _) = get(p, "/hub/inspect?path=shop", &with);
    assert_eq!(s, 400);
    // So are network paths: looking at one would connect to its host (H-1).
    for unc in [r"\\attacker.example\x", "//attacker.example/x"] {
        let (s, _, body) = get(p, &format!("/hub/inspect?path={}", enc(unc)), &with);
        assert_eq!(s, 400, "{unc}: {body}");
    }
    // And a page on another loopback port, which the browser deems same-site,
    // learns nothing (H-1).
    let foreign = with.replace("same-origin", "same-site");
    let (s, _, _) = get(p, &format!("/hub/inspect?path={}", enc(&dir)), &foreign);
    assert_eq!(s, 403);
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A WebSocket handshake on `/hub/ws?path=<path>`: status, headers lowercased
/// and, after a 101, the payload of the first text frame.
fn ws_open(port: u16, path: &str, extra: &str) -> (u16, String, Option<String>) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET /hub/ws?path={} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n",
        enc(path)
    )
    .unwrap();
    let mut r = BufReader::new(s);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let status = head.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
    if status != 101 {
        return (status, head.to_ascii_lowercase(), None);
    }
    let mut hdr = [0u8; 2];
    r.read_exact(&mut hdr).unwrap();
    assert_eq!(hdr[0], 0x81, "a final text frame");
    let len = match hdr[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b).unwrap();
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).unwrap();
    (
        status,
        head.to_ascii_lowercase(),
        Some(String::from_utf8(payload).unwrap()),
    )
}

#[test]
fn live_streams_come_over_websocket() {
    let h = start();
    let p = h.port;
    let cookie = format!("Cookie: {}\r\n", paired_cookie(&h));
    let own = format!("Origin: http://127.0.0.1:{p}\r\n");

    // Paired, from the hub's own page: the hub's stream, one event per message.
    let (s, head, first) = ws_open(p, "/hub/events", &format!("{cookie}{own}"));
    assert_eq!(s, 101, "{head}");
    let v: serde_json::Value = serde_json::from_str(&first.unwrap()).unwrap();
    assert!(
        ["stacks", "docker", "release", "update"].contains(&v["type"].as_str().unwrap()),
        "{v}"
    );

    // ADR-0019: a page of another origin, or no pairing, gets nothing.
    let (s, _, _) = ws_open(
        p,
        "/hub/events",
        &format!("{cookie}Origin: http://evil.example\r\n"),
    );
    assert_eq!(s, 403);
    let (s, _, _) = ws_open(p, "/hub/events", &cookie);
    assert_eq!(s, 403, "no Origin at all");
    let (s, _, _) = ws_open(p, "/hub/events", &own);
    assert_eq!(s, 401);

    // A stack that is not running answers the handshake, as its stream did.
    let (s, _, _) = ws_open(
        p,
        "/hub/stacks/nope/api/v1/events",
        &format!("{cookie}{own}"),
    );
    assert_eq!(s, 503);
    let (s, _, _) = ws_open(p, "/elsewhere", &format!("{cookie}{own}"));
    assert_eq!(s, 400);
}

#[test]
fn an_update_needs_the_hubs_own_page() {
    let h = start();
    let p = h.port;
    let cookie = format!(
        "Cookie: {}
",
        paired_cookie(&h)
    );
    // Installing a release is a mutation: never from another origin, never
    // without the pairing. (A request that passes would really update.)
    for extra in [
        format!(
            "{cookie}Origin: http://evil.example
"
        ),
        cookie.clone(),
        format!(
            "Origin: http://127.0.0.1:{p}
"
        ),
    ] {
        let (s, _, _) = http(
            p,
            &format!(
                "POST /hub/update HTTP/1.1
Host: 127.0.0.1:{p}
{extra}Content-Length: 0
Connection: close

"
            ),
        );
        assert!(s == 401 || s == 403, "{extra}: {s}");
    }
}

/// "Check for updates" asks GitHub, so only the hub's own page may ask, and a
/// machine that turned checks off says so instead of asking.
#[test]
fn a_check_for_updates_says_when_checks_are_off() {
    let h = start();
    let p = h.port;
    let cookie = paired_cookie(&h);
    let ask = |extra: &str| {
        http(
            p,
            &format!(
                "POST /hub/update/check HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
    };
    let (s, _, _) = ask(&format!(
        "Cookie: {cookie}\r\nOrigin: http://evil.example\r\n"
    ));
    assert_eq!(s, 403);
    let (s, _, body) = ask(&format!(
        "Cookie: {cookie}\r\nOrigin: http://127.0.0.1:{p}\r\n"
    ));
    assert_eq!(s, 409, "{body}");
    assert!(body.contains("DEVSTACK_NO_UPDATE_CHECK"), "{body}");
}

/// #11: a start from the dashboard that fails before the daemon runs (here a
/// stack file that needs a newer devstack) is reported on the stack's entry,
/// instead of leaving the page on "Starting…".
#[test]
fn a_failed_start_is_reported() {
    let h = start();
    let p = h.port;
    let with = format!(
        "Cookie: {}\r\nSec-Fetch-Site: same-origin\r\n",
        paired_cookie(&h)
    );
    let own = format!("Origin: http://127.0.0.1:{p}\r\n");
    let post = |path: &str, body: &str| {
        http(
            p,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}{own}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let shop = h.home.path().join("future");
    std::fs::create_dir_all(&shop).unwrap();
    let file = shop.join("devstack.toml");
    std::fs::write(&file, "version = 1\n[stack]\nname = \"future\"\n").unwrap();
    let dir = serde_json::to_string(&shop.to_string_lossy()).unwrap();
    let (s, _, body) = post("/hub/stacks/add", &format!("{{\"path\":{dir}}}"));
    assert_eq!(s, 200, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Registered; now the file asks for a devstack that does not exist yet.
    std::fs::write(
        &file,
        "version = 1\n[stack]\nname = \"future\"\nrequires = \"99.0.0\"\n",
    )
    .unwrap();
    let (s, _, body) = post(&format!("/hub/stacks/{id}/up"), "");
    assert_eq!(s, 202, "{body}");
    let t0 = Instant::now();
    loop {
        let (_, _, list) = get(p, "/hub/stacks", &with);
        let entry = serde_json::from_str::<serde_json::Value>(&list)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id.as_str())
            .cloned()
            .unwrap_or_default();
        if let Some(why) = entry["start_error"].as_str() {
            assert!(why.contains("99.0.0"), "{why}");
            assert_eq!(entry["running"], false);
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "no start_error: {list}"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// A2: `down`, then `up` at once, from the dashboard. The stack runs again,
/// with a new daemon that stays; a request in between is answered, or told
/// that the stack is stopping or starting (`503`), never a `502`.
#[test]
fn down_then_up_at_once_runs_again() {
    let h = start();
    let p = h.port;
    let with = format!(
        "Cookie: {}\r\nSec-Fetch-Site: same-origin\r\n",
        paired_cookie(&h)
    );
    let own = format!("Origin: http://127.0.0.1:{p}\r\n");
    let post = |path: &str, body: &str| {
        http(
            p,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}{own}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let repo = h.home.path().join("again");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("devstack.toml"),
        "version = 1\n[stack]\nname = \"again\"\n",
    )
    .unwrap();
    let dir = serde_json::to_string(&repo.to_string_lossy()).unwrap();
    let (s, _, body) = post("/hub/stacks/add", &format!("{{\"path\":{dir}}}"));
    assert_eq!(s, 200, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let daemon_pid = || -> Option<u64> {
        let (_, _, list) = get(p, "/hub/stacks", &with);
        serde_json::from_str::<serde_json::Value>(&list)
            .ok()?
            .as_array()?
            .iter()
            .find(|e| e["id"] == id.as_str())?["daemon"]["pid"]
            .as_u64()
    };
    let wait_for = |what: &str, cond: &dyn Fn() -> bool| {
        let t0 = Instant::now();
        while !cond() {
            assert!(t0.elapsed() < Duration::from_secs(60), "{what}");
            let (s, head, body) = get(p, &format!("/hub/stacks/{id}/api/v1/stack"), &with);
            assert!(s == 200 || s == 503, "{s} {head} {body}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let (s, _, body) = post(&format!("/hub/stacks/{id}/up"), "");
    assert_eq!(s, 202, "{body}");
    wait_for("first start", &|| daemon_pid().is_some());
    let first = daemon_pid().unwrap();

    let (s, _, body) = post(&format!("/hub/stacks/{id}/down"), "");
    assert_eq!(s, 202, "{body}");
    let (s, _, body) = post(&format!("/hub/stacks/{id}/up"), "");
    assert_eq!(s, 202, "{body}");
    wait_for("the restart", &|| daemon_pid().is_some_and(|d| d != first));
    let second = daemon_pid().unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(daemon_pid(), Some(second), "the new daemon stays");
    let (s, _, body) = get(p, &format!("/hub/stacks/{id}/api/v1/stack"), &with);
    assert_eq!(s, 200, "{body}");

    let (s, _, body) = post(&format!("/hub/stacks/{id}/down"), "");
    assert_eq!(s, 202, "{body}");
    let state = repo.join(".devstack").join("daemon.json");
    let t0 = Instant::now();
    while state.exists() && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// #169: the hub offers the pool nothing and says why; the share switch is
/// gone until it starts a node agent (#171).
#[test]
fn the_hub_offers_the_pool_nothing_and_says_why() {
    let h = start();
    let p = h.port;
    let bearer = format!("Authorization: Bearer {}\r\n", token(&h));
    let (s, _, body) = get(p, "/hub/pool/share", &bearer);
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let why = v["unavailable"].as_str().unwrap_or("");
    assert!(why.contains("#171") && !why.contains("  "), "{v}");
    assert!(
        v.get("settings").is_none() && v.get("offer").is_none(),
        "{v}"
    );
    let body = r#"{"enabled":true}"#;
    let (s, _, _) = http(
        p,
        &format!(
            "PUT /hub/pool/share HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{bearer}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert_eq!(s, 405, "no switch to turn on");
}

/// ADR-0045: the settings over the API, and a link from a notification
/// (`/auth?next=`) that lands on the stack, without a second session for a
/// browser that is paired already.
#[test]
fn notification_settings_and_the_link_a_click_opens() {
    let h = start();
    let p = h.port;
    let bearer = format!("Authorization: Bearer {}\r\n", token(&h));
    let send = |method: &str, path: &str, body: &str| {
        http(
            p,
            &format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{bearer}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let (s, _, body) = get(p, "/hub/notify", &bearer);
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["available"], false);
    assert!(
        v["unavailable"]
            .as_str()
            .unwrap()
            .contains("DEVSTACK_NO_NOTIFY"),
        "{v}"
    );
    let on = |v: &serde_json::Value, k: &str| {
        v["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["kind"] == k)
            .unwrap()["on"]
            .clone()
    };
    assert_eq!(on(&v, "build_failed"), true);
    assert_eq!(on(&v, "live"), false);

    let (s, _, body) = send(
        "PUT",
        "/hub/notify",
        r#"{"kinds":{"live":true,"held":false},"muted":["ab12/agent-1"]}"#,
    );
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (on(&v, "live"), on(&v, "held")),
        (true.into(), false.into())
    );
    assert_eq!(v["muted"], serde_json::json!(["ab12/agent-1"]));
    let (s, _, _) = send("PUT", "/hub/notify", r#"{"kinds":{"everything":true}}"#);
    assert_eq!(s, 400);
    let (s, _, _) = send("PUT", "/hub/notify", r#"{"muted":["../x"]}"#);
    assert_eq!(s, 400);
    // Nothing would show: the test says why.
    let (s, _, body) = send("POST", "/hub/notify/test", "");
    assert_eq!(s, 503);
    assert!(body.contains("DEVSTACK_NO_NOTIFY"), "{body}");
    // With the cookie, the settings need the hub's own page as any mutation.
    let cookie = paired_cookie(&h);
    let (s, _, _) = http(
        p,
        &format!(
            "PUT /hub/notify HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nCookie: {cookie}\r\nOrigin: http://evil.example\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        ),
    );
    assert_eq!(s, 403);

    // A click: pairing with `next`, which names a dashboard route.
    let next = "%2F%23%2Fs%2Fab12%2Fservices%2Fapi%3Flane%3Dfeat-x";
    let (s, head, _) = get(p, &format!("/auth?code={}&next={next}", pair(&h)), "");
    assert_eq!(s, 303, "{head}");
    assert!(
        head.contains("location: /#/s/ab12/services/api?lane=feat-x"),
        "{head}"
    );
    assert!(head.contains("set-cookie:"), "{head}");
    // Paired already: on to the route, no new session.
    let (s, head, _) = get(
        p,
        &format!("/auth?code={}&next={next}", pair(&h)),
        &format!("Cookie: {cookie}\r\n"),
    );
    assert_eq!(s, 303, "{head}");
    assert!(
        head.contains("location: /#/s/ab12/services/api?lane=feat-x"),
        "{head}"
    );
    assert!(!head.contains("set-cookie:"), "{head}");
    // Anything but a dashboard route goes home.
    let (_, head, _) = get(
        p,
        &format!("/auth?code={}&next=https%3A%2F%2Fevil.example%2F", pair(&h)),
        "",
    );
    assert!(
        head.contains("location: /\r\n") || head.ends_with("location: /"),
        "{head}"
    );
}

/// ADR-0045: the hub follows a running stack by itself, no dashboard open,
/// and a service that keeps exiting is one notice (folded), not one per
/// restart.
#[test]
fn a_service_that_keeps_exiting_is_one_notice() {
    let h = start();
    let p = h.port;
    let with = format!(
        "Cookie: {}\r\nSec-Fetch-Site: same-origin\r\n",
        paired_cookie(&h)
    );
    let own = format!("Origin: http://127.0.0.1:{p}\r\n");
    let post = |path: &str, body: &str| {
        http(
            p,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\n{with}{own}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let repo = h.home.path().join("crashy");
    std::fs::create_dir_all(&repo).unwrap();
    let brief = if cfg!(windows) {
        "ping -n 2 127.0.0.1 >NUL"
    } else {
        "sleep 1"
    };
    std::fs::write(
        repo.join("devstack.toml"),
        format!("version = 1\n[stack]\nname = \"crashy\"\n[services.api]\nrun = \"{brief}\"\n"),
    )
    .unwrap();
    let dir = serde_json::to_string(&repo.to_string_lossy()).unwrap();
    let (s, _, body) = post("/hub/stacks/add", &format!("{{\"path\":{dir}}}"));
    assert_eq!(s, 200, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _, body) = post(&format!("/hub/stacks/{id}/up"), "");
    assert_eq!(s, 202, "{body}");

    let log = h.home.path().join("devstack").join("hub").join("hub.log");
    let notices = || -> Vec<String> {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"notice\""))
            .map(str::to_string)
            .collect()
    };
    let daemon_log = repo.join(".devstack").join("daemon.log");
    let restarts = || {
        std::fs::read_to_string(&daemon_log)
            .unwrap_or_default()
            .matches("\"service_restarting\"")
            .count()
    };
    let t0 = Instant::now();
    while restarts() < 2 {
        assert!(
            t0.elapsed() < Duration::from_secs(90),
            "the service did not exit twice; notices: {:?}",
            notices()
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let seen = notices();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let n: serde_json::Value = serde_json::from_str(&seen[0]).unwrap();
    assert_eq!(n["kind"], "service_failed", "{n}");
    assert_eq!(n["service"], "api", "{n}");
    assert_eq!(n["stack"], "crashy", "{n}");
    assert_eq!(n["shown"], false, "{n}");

    let (s, _, body) = post(&format!("/hub/stacks/{id}/down"), "");
    assert_eq!(s, 202, "{body}");
    let state = repo.join(".devstack").join("daemon.json");
    let t0 = Instant::now();
    while state.exists() && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
    }
}
