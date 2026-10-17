//! A requester gets a lease from the coordinator, the node admits it, and
//! the requester syncs a tree to the node's mirror by content (ADR-0029,
//! ADR-0031 point 4).

use devstack_node::coordinator::{self, LeaseRequest, NodeRef};
use devstack_node::member::Local;
use devstack_node::node::{self, NodeConfig};
use devstack_node::nodeclient::{ClientError, NodeClient};
use devstack_node::share::{Amount, Settings};
use devstack_node::store::{SHELL, Store};
use std::path::Path;
use std::time::Duration;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// A coordinator in `dir` on a free loopback port.
fn coordinator(dir: &Path) -> (String, String) {
    let fp = coordinator::init(dir, &["127.0.0.1".into()]).unwrap();
    let (url, stop) = coordinator_at(dir, 0);
    std::mem::forget(stop);
    (url, fp)
}

/// The coordinator in `dir`, served on `port` (0: a free one) until the
/// returned stop is called.
fn coordinator_at(dir: &Path, port: u16) -> (String, impl FnOnce()) {
    let rt = rt();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))
        .unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let dir = dir.to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let t = std::thread::spawn(move || {
        rt.block_on(coordinator::serve(&dir, listener, async {
            let _ = rx.await;
        }))
        .unwrap();
    });
    (url, move || {
        let _ = tx.send(());
        t.join().unwrap();
    })
}

/// Wait until `f` holds, at most `secs`.
fn wait_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(std::time::Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A machine in `dir` that joined and was approved.
fn member(coord: &Path, url: &str, fp: &str, dir: &Path, name: &str) -> Local {
    let l = Local::new(dir);
    let m = l.join(url, fp, name).unwrap();
    Store::new(coord)
        .approve(&m.id, &coordinator::ca(coord).unwrap(), None, SHELL)
        .unwrap();
    l.refresh().unwrap();
    l
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn a_lane_is_leased_admitted_and_synced() {
    let c = tempfile::tempdir().unwrap();
    let (url, fp) = coordinator(c.path());
    let (nd, rd, od) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let node_member = member(c.path(), &url, &fp, nd.path(), "node-pc");
    let requester = member(c.path(), &url, &fp, rd.path(), "dev-pc");
    let other = member(c.path(), &url, &fp, od.path(), "other-pc");

    // The node agent, sharing at most 4 cores and 4 GiB.
    let rt_node = rt();
    let listener = rt_node
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let api = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let config = NodeConfig {
        dir: nd.path().to_path_buf(),
        api: api.clone(),
        share: Settings {
            enabled: true,
            max_cpus: Some(4.0),
            max_memory: Some(4 << 30),
        },
        dedicated: true,
    };
    std::thread::spawn(move || {
        rt_node
            .block_on(node::serve(config, listener, std::future::pending()))
            .unwrap();
    });
    let node_id = node_member.membership().unwrap().unwrap().id;
    // Online once it has reported; its API is taken only once an admin
    // granted it to run lanes (ADR-0032, addendum).
    wait_for(10, "the node reports", || {
        requester
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.id == node_id && n.online)
    });
    let ns = requester.nodes().unwrap();
    let n = ns.iter().find(|n| n.id == node_id).unwrap();
    assert!(n.report.as_ref().unwrap().api.is_none(), "not granted yet");
    assert_eq!(n.kind, "machine");
    let not_yet = requester.lease(&LeaseRequest {
        node: "node-pc".into(),
        repo: "shop".into(),
        lane: "early".into(),
        need: Amount::default(),
    });
    assert!(not_yet.unwrap_err().to_string().starts_with("409"));
    assert!(
        requester
            .lease(&LeaseRequest {
                node: "pool".into(),
                repo: "shop".into(),
                lane: "early".into(),
                need: Amount::default(),
            })
            .is_err(),
        "the pool places on granted nodes only"
    );
    Store::new(c.path())
        .set_node(&node_id, true, SHELL)
        .unwrap();
    let mut online = false;
    for _ in 0..150 {
        let ns = requester.nodes().unwrap();
        if ns.iter().any(|n| {
            n.id == node_id && n.online && n.report.as_ref().is_some_and(|r| r.api.is_some())
        }) {
            online = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(online, "the node reports to the coordinator");

    // More than it shares: the coordinator says no.
    let too_much = requester.lease(&LeaseRequest {
        node: "node-pc".into(),
        repo: "shop".into(),
        lane: "big".into(),
        need: Amount {
            cpus: 64.0,
            memory: 1 << 40,
        },
    });
    assert!(too_much.unwrap_err().to_string().starts_with("409"));

    // No node named: the coordinator places it, on the only node that runs
    // lanes (ADR-0031 point 4).
    let placed = requester
        .lease(&LeaseRequest {
            node: "pool".into(),
            repo: "shop".into(),
            lane: "placed".into(),
            need: Amount {
                cpus: 0.1,
                memory: 64 << 20,
            },
        })
        .unwrap();
    assert_eq!(placed.node.id, node_id, "the coordinator picks the node");

    let offer = requester
        .lease(&LeaseRequest {
            node: "node-pc".into(),
            repo: "shop".into(),
            lane: "login-fix".into(),
            need: Amount {
                cpus: 0.1,
                memory: 64 << 20,
            },
        })
        .unwrap();
    assert_eq!(offer.node.api, api);
    let rt = rt();
    let client = NodeClient::new(&requester, offer.node.clone()).unwrap();
    let lease = rt.block_on(client.admit(&offer)).unwrap();
    assert_eq!(lease.lane, "login-fix");

    // Another member cannot take the lease over, renew or sync it.
    let theirs = NodeClient::new(&other, offer.node.clone()).unwrap();
    assert!(matches!(
        rt.block_on(theirs.admit(&offer)),
        Err(ClientError::Status(409, _))
    ));
    assert!(matches!(
        rt.block_on(theirs.renew(&lease.id)),
        Err(ClientError::Status(404, _))
    ));

    // A node is known by its key: a client that expects another member at
    // that address does not talk to it.
    let wrong = NodeClient::new(
        &requester,
        NodeRef {
            id: "0000000000000000".into(),
            ..offer.node.clone()
        },
    )
    .unwrap();
    assert!(matches!(
        rt.block_on(wrong.node()),
        Err(ClientError::Tls(_))
    ));

    // First sync: everything.
    let tree = tempfile::tempdir().unwrap();
    write(tree.path(), "devstack.toml", "[stack]\nname = \"shop\"\n");
    write(tree.path(), "src/Api/Program.cs", "// api\n");
    write(tree.path(), "src/Web/app.ts", "export {};\n");
    let files = |t: &Path| -> Vec<String> {
        ["devstack.toml", "src/Api/Program.cs", "src/Web/app.ts"]
            .iter()
            .filter(|f| t.join(f).exists())
            .map(|f| f.to_string())
            .collect()
    };
    let s1 = rt
        .block_on(client.sync(&lease.id, tree.path(), &files(tree.path())))
        .unwrap();
    assert_eq!(
        (s1.files, s1.uploaded, s1.written, s1.removed),
        (3, 3, 3, 0)
    );
    let mirror = nd.path().join("node/lanes").join(&lease.id).join("src");
    assert_eq!(
        std::fs::read_to_string(mirror.join("src/Api/Program.cs")).unwrap(),
        "// api\n"
    );

    // An edit and a deletion: one upload, one write, one removal.
    write(tree.path(), "src/Api/Program.cs", "// api, edited\n");
    std::fs::remove_file(tree.path().join("src/Web/app.ts")).unwrap();
    let s2 = rt
        .block_on(client.sync(&lease.id, tree.path(), &files(tree.path())))
        .unwrap();
    assert_eq!(
        (s2.files, s2.uploaded, s2.written, s2.removed),
        (2, 1, 1, 1)
    );
    assert_eq!(
        std::fs::read_to_string(mirror.join("src/Api/Program.cs")).unwrap(),
        "// api, edited\n"
    );
    assert!(!mirror.join("src/Web").exists());

    // Nothing changed: nothing goes over.
    let s3 = rt
        .block_on(client.sync(&lease.id, tree.path(), &files(tree.path())))
        .unwrap();
    assert_eq!((s3.uploaded, s3.written, s3.removed), (0, 0, 0));

    let renewed = rt.block_on(client.renew(&lease.id)).unwrap();
    assert!(renewed.expires >= lease.expires);
    let view = rt.block_on(client.node()).unwrap();
    assert_eq!(view.leases.len(), 1);

    // Shared (ADR-0035): nobody sees it until its owner shares it; then
    // the others see it, and the node lets a tunnel through to a shared
    // port only (this lane was never started: past the check, it answers
    // that it does not run).
    use devstack_node::coordinator::{SharedService, Sharing};
    assert!(other.shared().unwrap().is_empty());
    assert!(matches!(
        rt.block_on(theirs.open_shared(&lease.id, 5173)),
        Err(ClientError::Status(403, _))
    ));
    let sharing = Sharing {
        with: vec!["*".into()],
        services: vec![SharedService {
            name: "web".into(),
            port: 5173,
            scheme: "http".into(),
            here: 5273,
        }],
    };
    assert!(
        other.share_lease(&lease.id, Some(&sharing)).is_err(),
        "only its owner shares a lane"
    );
    requester.share_lease(&lease.id, Some(&sharing)).unwrap();
    // Listed once the node reported the lease running.
    let mut seen = Vec::new();
    for _ in 0..100 {
        seen = other.shared().unwrap();
        if !seen.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(seen.len(), 1, "the other member sees the shared lane");
    assert_eq!(
        (
            seen[0].lane.as_str(),
            seen[0].owner_name.as_str(),
            seen[0].node.id.as_str()
        ),
        ("login-fix", "dev-pc", node_id.as_str())
    );
    assert_eq!(seen[0].services, sharing.services);
    assert!(requester.shared().unwrap().is_empty(), "not one's own");
    assert!(matches!(
        rt.block_on(theirs.open_shared(&lease.id, 5432)),
        Err(ClientError::Status(403, _))
    ));
    assert!(matches!(
        rt.block_on(theirs.open_shared(&lease.id, 5173)),
        Err(ClientError::Status(409, _))
    ));
    requester.share_lease(&lease.id, None).unwrap();
    assert!(other.shared().unwrap().is_empty());
    assert!(matches!(
        rt.block_on(theirs.open_shared(&lease.id, 5173)),
        Err(ClientError::Status(403, _))
    ));

    rt.block_on(client.release(&lease.id)).unwrap();
    // The node clears a lane off the request's thread (Docker takes its time).
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while mirror.exists() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !mirror.exists(),
        "a released lease takes its mirror with it"
    );
}

/// An admin pauses a node (no new lanes) and ends a lease; the node drops
/// the lane at its next report (ADR-0032 point 4).
#[test]
fn an_admin_pauses_a_node_and_ends_a_lease() {
    let c = tempfile::tempdir().unwrap();
    let fp = coordinator::init(c.path(), &["127.0.0.1".into()]).unwrap();
    let (url, stop) = coordinator_at(c.path(), 0);
    let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
    let (nd, rd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let node_member = member(c.path(), &url, &fp, nd.path(), "node-pc");
    let admin = member(c.path(), &url, &fp, rd.path(), "admin-pc");
    let admin_id = admin.membership().unwrap().unwrap().id;
    Store::new(c.path())
        .set_role(&admin_id, devstack_node::store::Role::Admin, SHELL)
        .unwrap();
    let node_id = node_member.membership().unwrap().unwrap().id;
    admin.admin_node(&node_id, true).unwrap();

    let rt_node = rt();
    let listener = rt_node
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let api = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let config = NodeConfig {
        dir: nd.path().to_path_buf(),
        api,
        share: Settings {
            enabled: true,
            max_cpus: Some(4.0),
            max_memory: Some(4 << 30),
        },
        dedicated: true,
    };
    std::thread::spawn(move || {
        rt_node
            .block_on(node::serve(config, listener, std::future::pending()))
            .unwrap();
    });
    wait_for(10, "the node reports", || {
        admin
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.id == node_id && n.online)
    });
    let ask = || {
        admin.lease(&LeaseRequest {
            node: "node-pc".into(),
            repo: "shop".into(),
            lane: "fix".into(),
            need: Amount {
                cpus: 0.1,
                memory: 64 << 20,
            },
        })
    };

    // A policy (ADR-0033): only members tagged `team:x`.
    let store = Store::new(c.path());
    store
        .set_policy(
            &node_id,
            Some(devstack_node::policy::NodePolicy {
                allow: vec!["team:x".into()],
                ..Default::default()
            }),
            SHELL,
        )
        .unwrap();
    let e = ask().unwrap_err().to_string();
    assert!(e.contains("team:x"), "{e}");
    store
        .set_tags(&admin_id, &["team:x".into()], SHELL)
        .unwrap();
    assert!(ask().is_ok(), "a member with the tag may");
    store.set_policy(&node_id, None, SHELL).unwrap();

    admin.admin_pause(&node_id, true).unwrap();
    assert!(
        admin
            .nodes()
            .unwrap()
            .iter()
            .any(|n| n.id == node_id && n.paused)
    );
    let e = ask().unwrap_err().to_string();
    assert!(e.contains("paused"), "{e}");
    admin.admin_pause(&node_id, false).unwrap();

    let offer = ask().unwrap();
    let rt = rt();
    let client = NodeClient::new(&admin, offer.node.clone()).unwrap();
    let lease = rt.block_on(client.admit(&offer)).unwrap();
    // The coordinator knows it once the node reported it.
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while !admin
        .leases()
        .unwrap()
        .iter()
        .any(|l| l.id == lease.id && l.state == "running")
    {
        assert!(
            std::time::Instant::now() < until,
            "the node reports its lease"
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    // The coordinator restarts: it knows the lease again from the node's
    // next report, and an admin can still end it.
    stop();
    let (_, stop) = coordinator_at(c.path(), port);
    wait_for(30, "the coordinator knows the lease again", || {
        admin
            .leases()
            .is_ok_and(|ls| ls.iter().any(|l| l.id == lease.id && l.state == "running"))
    });

    admin.admin_end(&lease.id).unwrap();
    // Another machine reports in the meantime (every hub does, without
    // leases): the end still reaches the node that holds the lease.
    admin
        .report(&devstack_node::coordinator::Report::default())
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(40);
    loop {
        match rt.block_on(client.renew(&lease.id)) {
            Err(ClientError::Status(404, _)) => break,
            _ => {
                assert!(
                    std::time::Instant::now() < until,
                    "the node drops an ended lease"
                );
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    let log: Vec<String> = admin
        .admin_audit()
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(&log[..3], ["end", "resume", "pause"]);
    assert!(log.contains(&"policy".to_string()) && log.contains(&"tags".to_string()));
    assert!(log.contains(&"node".to_string()));
    stop();
}
