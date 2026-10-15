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
    let (s, _, _) = get(p, "/stacks/abc/services/api", "");
    assert_eq!(s, 200, "SPA fallback");

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

    let with = format!("Cookie: {cookie}\r\n");
    let (s, _, body) = get(p, "/hub/session", &with);
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("version"));

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
    let with = format!("Cookie: {}\r\n", paired_cookie(&h));

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
    let stored =
        std::fs::read_to_string(h2.home.path().join("devstack/hub/hub-sessions.json")).unwrap();
    assert!(
        !stored.contains(with.trim_start_matches("Cookie: devstack_hub=").trim()),
        "cookie stored in clear"
    );
}
