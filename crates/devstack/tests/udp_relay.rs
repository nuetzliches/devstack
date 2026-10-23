//! A UDP port of a lane on a node, relayed (ADR-0056), end to end at the
//! tunnel: this machine's relay ([`devstack_daemon::udprelay::relay`]) opens
//! a session as an HTTP upgrade (`devstack-udp`) on a real lane daemon's
//! `/v1/tunnel/{port}`, which sends the datagrams to the container's port
//! and the answers back. The lane daemon reads its containers from the
//! node agent's file (`DEVSTACK_EXTERNAL_INFRA`), here naming an echo
//! socket of this test instead of a container: no Docker and no node, so it
//! runs on every OS. Between the two the node agent splices any upgrade
//! without reading it (`pass` in `devstack-node`), as for TCP tunnels.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

const BIN: &str = env!("CARGO_BIN_EXE_devstack");

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The lane daemon's API port and token, once it answers.
fn daemon(repo: &Path, home: &Path) -> (u16, String) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let info = std::fs::read_to_string(repo.join(".devstack/daemon.json")).ok();
        let token = std::fs::read_to_string(home.join("devstack/token")).ok();
        if let (Some(info), Some(token)) = (info, token) {
            let v: serde_json::Value = serde_json::from_str(&info).unwrap_or_default();
            if let Some(port) = v["port"].as_u64()
                && std::net::TcpStream::connect(("127.0.0.1", port as u16)).is_ok()
            {
                return (port as u16, token.trim().to_string());
            }
        }
        assert!(Instant::now() < until, "the lane daemon does not answer");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `GET /v1/tunnel/{port}` with `Upgrade: protocol` on the daemon: the
/// upgraded stream, or the status and body it answered instead.
async fn open(
    api: u16,
    token: &str,
    port: u16,
    protocol: &str,
) -> Result<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>, String> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", api))
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn.with_upgrades());
    let req = hyper::Request::get(format!("/v1/tunnel/{port}"))
        .header(hyper::header::HOST, format!("127.0.0.1:{api}"))
        .header(hyper::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(hyper::header::CONNECTION, "upgrade")
        .header(hyper::header::UPGRADE, protocol)
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();
    let mut res = send.send_request(req).await.map_err(|e| e.to_string())?;
    if res.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
        use http_body_util::BodyExt;
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        return Err(format!("{status} {}", String::from_utf8_lossy(&body)));
    }
    let up = hyper::upgrade::on(&mut res)
        .await
        .map_err(|e| e.to_string())?;
    Ok(hyper_util::rt::TokioIo::new(up))
}

async fn ask(client: &UdpSocket, to: std::net::SocketAddr, what: &[u8]) -> Vec<u8> {
    // macOS sends no more than 9 KiB unless asked to.
    devstack_daemon::udprelay::room_for_any_datagram(client);
    client.send_to(what, to).await.unwrap();
    let mut buf = vec![0u8; 1 << 16];
    let (n, _) = tokio::time::timeout(Duration::from_secs(10), client.recv_from(&mut buf))
        .await
        .expect("an answer through the tunnel")
        .unwrap();
    buf.truncate(n);
    buf
}

#[test]
fn a_datagram_goes_through_a_lanes_tunnel_and_its_answer_comes_back() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (repo, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // The "container": an echo socket, at the address the lane's network
    // would give it.
    let echo = rt.block_on(UdpSocket::bind("127.0.0.1:0")).unwrap();
    devstack_daemon::udprelay::room_for_any_datagram(&echo);
    let echo_at = echo.local_addr().unwrap();
    rt.spawn(async move {
        let mut buf = vec![0u8; 1 << 16];
        while let Ok((n, from)) = echo.recv_from(&mut buf).await {
            let mut reply = b"echo:".to_vec();
            reply.extend_from_slice(&buf[..n]);
            let _ = echo.send_to(&reply, from).await;
        }
    });

    // What the node agent writes for the lane daemon: `dns` publishes 53/udp
    // on 5453 for the developer, 53/tcp beside it and 8080 on 8180; the
    // requester relays UDP (`udp`).
    let file = repo.path().join("external-infra.json");
    std::fs::write(
        &file,
        serde_json::json!({
            "udp": true,
            "groups": { "app": { "project": "udp-it", "state": "up", "services": { "dns": {
                "ports": { "53": "127.0.0.1:9", "53/udp": echo_at.to_string(), "8080": "127.0.0.1:9" },
                "published": { "53": 5453, "53/udp": 5453, "8080": 8180 },
                "state": "running", "health": ""
            }}}}
        })
        .to_string(),
    )
    .unwrap();
    let stack = repo.path().join("devstack.toml");
    std::fs::write(&stack, "version = 1\n[stack]\nname = \"udp-it\"\n").unwrap();
    let _daemon = Kill(
        Command::new(BIN)
            .args(["daemon", "-f"])
            .arg(&stack)
            .current_dir(repo.path())
            .env("LOCALAPPDATA", home.path())
            .env("XDG_STATE_HOME", home.path())
            .env("DEVSTACK_EXTERNAL_INFRA", &file)
            .env("DEVSTACK_NO_HUB", "1")
            .env("DEVSTACK_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (api, token) = daemon(repo.path(), home.path());

    rt.block_on(async {
        // Only a container's UDP port is a UDP tunnel: not a TCP one, not
        // a port of nothing.
        for port in [8180, 5454] {
            match open(api, &token, port, devstack_daemon::udprelay::PROTOCOL).await {
                Err(e) => assert!(e.starts_with("403"), "{port}: {e}"),
                Ok(_) => panic!("{port} is no UDP port of the lane"),
            }
        }

        // This machine's relay, on a port of its own, its sessions opened
        // on the lane daemon's tunnel.
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_at = socket.local_addr().unwrap();
        let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (count, token2) = (opened.clone(), token.clone());
        let opener = move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let token = token2.clone();
            async move {
                let stream = open(api, &token, 5453, devstack_daemon::udprelay::PROTOCOL).await?;
                Ok(devstack_daemon::udprelay::Opened {
                    stream,
                    closed: None,
                })
            }
        };
        let relay = tokio::spawn(devstack_daemon::udprelay::relay(
            socket,
            opener,
            devstack_daemon::udprelay::Limits::default(),
            Arc::new(|e| panic!("a tunnel did not open: {e}")),
        ));

        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(ask(&a, relay_at, b"who is db?").await, b"echo:who is db?");
        assert_eq!(ask(&a, relay_at, b"again").await, b"echo:again");
        // A datagram larger than any link's MTU arrives whole.
        let big = vec![b'x'; 32_000];
        let back = ask(&a, relay_at, &big).await;
        assert_eq!((back.len(), &back[..5]), (32_005, &b"echo:"[..]));
        assert_eq!(
            opened.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one client, one session"
        );
        assert_eq!(ask(&b, relay_at, b"me too").await, b"echo:me too");
        assert_eq!(
            opened.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a session per client"
        );
        relay.abort();
    });
}
