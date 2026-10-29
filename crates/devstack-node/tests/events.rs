//! The pool's event stream (ADR-0073): a member hears every part's version
//! first, then only the parts that changed; a member that is removed hears
//! the end of it; the parts' endpoints carry an `ETag`.

use devstack_node::coordinator;
use devstack_node::events::{self, Ended, Heard};
use devstack_node::member::Local;
use devstack_node::store::{Role, SHELL, Store};
use devstack_node::upstreams::{PoolAuth, PoolUpstream};
use std::path::Path;
use std::time::Duration;

fn coordinator(dir: &Path) -> (String, String) {
    let fp = coordinator::init(dir, &["127.0.0.1".into()]).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        rt.block_on(coordinator::serve(&dir, listener, std::future::pending()))
            .unwrap();
    });
    (url, fp)
}

fn member(coord: &Path, url: &str, fp: &str, dir: &Path, name: &str, role: Role) -> Local {
    let l = Local::new(dir);
    let m = l.join(url, fp, name).unwrap();
    Store::new(coord)
        .approve(&m.id, &coordinator::ca(coord).unwrap(), Some(role), SHELL)
        .unwrap();
    l.refresh().unwrap();
    l
}

/// Follow `local`'s pool on a thread of its own: what it hears, and how it
/// ended.
fn follow(local: Local) -> std::sync::mpsc::Receiver<Result<Heard, Ended>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let t = tx.clone();
        let ended = rt.block_on(events::follow(local, move |h| {
            let _ = t.send(Ok(h));
        }));
        let _ = tx.send(Err(ended));
    });
    rx
}

/// The next change heard within `within`, skipping connects and losses.
fn next_change(
    rx: &std::sync::mpsc::Receiver<Result<Heard, Ended>>,
    within: Duration,
) -> events::Versions {
    let until = std::time::Instant::now() + within;
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(Ok(Heard::Changed(v))) => return v,
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("following ended: {e:?}"),
            Err(_) => panic!("no change within {within:?}"),
        }
    }
}

#[test]
fn a_member_hears_every_part_first_then_what_changed_and_the_end_when_removed() {
    let c = tempfile::tempdir().unwrap();
    let (url, fp) = coordinator(c.path());
    let ad = tempfile::tempdir().unwrap();
    let admin = member(c.path(), &url, &fp, ad.path(), "lead-pc", Role::Admin);
    let md = tempfile::tempdir().unwrap();
    let dev = member(c.path(), &url, &fp, md.path(), "dev-pc", Role::Member);
    let dev_id = dev.membership().unwrap().unwrap().id;

    let rx = follow(Local::new(md.path()));
    let first = next_change(&rx, Duration::from_secs(10));
    assert_eq!(
        first.keys().map(String::as_str).collect::<Vec<_>>(),
        {
            let mut t = events::TOPICS.to_vec();
            t.sort_unstable();
            t
        },
        "every part first"
    );

    admin
        .admin_upstream_put(&PoolUpstream {
            name: "wiki".into(),
            url: "https://wiki.example.com/mcp".into(),
            auth: PoolAuth::Oauth {
                client_id: None,
                scopes: Vec::new(),
            },
            ..Default::default()
        })
        .unwrap();
    let changed = next_change(&rx, Duration::from_secs(10));
    assert!(changed.contains_key(events::UPSTREAMS), "{changed:?}");
    assert_ne!(changed[events::UPSTREAMS], first[events::UPSTREAMS]);
    assert!(!changed.contains_key(events::TELEMETRY), "{changed:?}");

    admin.admin_remove(&dev_id).unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(Err(Ended::NotMember(_) | Ended::NoMembership(_))) => break,
            Ok(Err(e)) => panic!("ended otherwise: {e:?}"),
            Ok(Ok(_)) => continue,
            Err(_) => panic!("a removed member still follows the pool"),
        }
    }
}

#[test]
fn a_part_comes_with_an_etag_and_unchanged_is_not_sent_again() {
    let c = tempfile::tempdir().unwrap();
    let (url, fp) = coordinator(c.path());
    let md = tempfile::tempdir().unwrap();
    let dev = member(c.path(), &url, &fp, md.path(), "dev-pc", Role::Member);
    let (m, agent) = dev.client().unwrap();
    for path in [
        "/v1/members",
        "/v1/nodes",
        "/v1/leases",
        "/v1/shared",
        "/v1/upstreams",
        "/v1/telemetry",
    ] {
        let first = agent
            .get(&format!("{}{path}", m.coordinator))
            .call()
            .unwrap();
        assert_eq!(first.status(), 200, "{path}");
        let tag = first
            .headers()
            .get("etag")
            .unwrap_or_else(|| panic!("{path}: no ETag"))
            .to_str()
            .unwrap()
            .to_string();
        let again = agent
            .get(&format!("{}{path}", m.coordinator))
            .header("If-None-Match", &tag)
            .call()
            .unwrap();
        assert_eq!(again.status(), 304, "{path}");
    }
}
