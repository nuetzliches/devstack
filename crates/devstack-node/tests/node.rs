//! A requester gets a lease from the coordinator, the node admits it, and
//! the requester syncs a tree to the node's mirror by content (ADR-0029,
//! ADR-0031 point 4).

use devstack_node::coordinator::{self, LeaseRequest, NodeRef};
use devstack_node::member::Local;
use devstack_node::node::{self, NodeConfig};
use devstack_node::nodeclient::{ClientError, NodeClient};
use devstack_node::share::{Need, Settings};
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
        on_update: None,
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
        need: Need::default(),
        need_from: None,
        floor: None,
        profile: None,
        devstack: None,
    });
    assert!(not_yet.unwrap_err().to_string().starts_with("409"));
    assert!(
        requester
            .lease(&LeaseRequest {
                node: "pool".into(),
                repo: "shop".into(),
                lane: "early".into(),
                need: Need::default(),
                need_from: None,
                floor: None,
                profile: None,
                devstack: None,
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
        need: Need {
            cpus: 64.0,
            memory: 1 << 40,
            ..Need::default()
        },
        need_from: None,
        floor: None,
        profile: None,
        devstack: None,
    });
    assert!(too_much.unwrap_err().to_string().starts_with("409"));

    // No node named: the coordinator places it, on the only node that runs
    // lanes (ADR-0031 point 4).
    let placed = requester
        .lease(&LeaseRequest {
            node: "pool".into(),
            repo: "shop".into(),
            lane: "placed".into(),
            need: Need {
                cpus: 0.1,
                memory: 64 << 20,
                ..Need::default()
            },
            need_from: None,
            floor: None,
            profile: None,
            devstack: None,
        })
        .unwrap();
    assert_eq!(placed.node.id, node_id, "the coordinator picks the node");

    let offer = requester
        .lease(&LeaseRequest {
            node: "node-pc".into(),
            repo: "shop".into(),
            lane: "login-fix".into(),
            need: Need {
                cpus: 0.1,
                memory: 64 << 20,
                ..Need::default()
            },
            need_from: None,
            floor: None,
            profile: None,
            devstack: None,
        })
        .unwrap();
    assert_eq!(offer.node.api, api);
    // The node says which devstack it runs; the coordinator passes it on.
    let this = Some(devstack_node::version::Release::this());
    assert_eq!(offer.node.devstack, this);
    assert_eq!(offer.coordinator, this);
    let rt = rt();
    let client = NodeClient::new(&requester, offer.node.clone()).unwrap();
    let lease = rt.block_on(client.admit(&offer)).unwrap();
    assert_eq!(lease.lane, "login-fix");
    assert_eq!(rt.block_on(client.node()).unwrap().devstack, this);

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
    // Nor reach its containers, which the node answers for (#101); for its
    // owner a lane not started has none yet.
    let infra = |c: &NodeClient| {
        let req = hyper::Request::get("/v1/infra/app/db/logs?tail=5")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();
        rt.block_on(c.forward(&lease.id, req))
            .map(|r| r.status().as_u16())
            .unwrap_or_else(|e| match e {
                ClientError::Status(s, _) => s,
                e => panic!("{e}"),
            })
    };
    assert_eq!(infra(&theirs), 404);
    assert_eq!(infra(&client), 404);

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
    // The lane's place is its slot (ADR-0038), named by the node.
    let slot = lease.slot.clone().expect("a slot");
    let mirror = nd.path().join("node/slots").join(&slot).join("src");
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
    let read = |c: &NodeClient, path: &str| {
        let req = hyper::Request::get(path)
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();
        rt.block_on(c.view(&lease.id, req))
            .map(|r| r.status().as_u16())
            .unwrap_or_else(|e| match e {
                ClientError::Status(s, _) => s,
                e => panic!("{e}"),
            })
    };
    assert!(matches!(
        rt.block_on(theirs.open_shared(&lease.id, 5173)),
        Err(ClientError::Status(403, _))
    ));
    // Not shared: not viewed either (ADR-0040).
    assert_eq!(read(&theirs, "/v1/stack"), 403);
    let sharing = Sharing::new(
        vec!["*".into()],
        vec![SharedService {
            name: "web".into(),
            port: 5173,
            scheme: "http".into(),
            here: 5273,
        }],
    );
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
    // Shared, it may be viewed as well (ADR-0040, amended 2026-10-20).
    // Past the check, the lane that never started answers that it does not
    // run; a change, or a path that is not for reading, stays refused.
    std::thread::sleep(Duration::from_secs(6)); // the node's last word, 5 s
    assert_eq!(read(&theirs, "/v1/stack"), 409);
    assert_eq!(read(&theirs, "/v1/services/api/env"), 403);
    assert_eq!(read(&theirs, "/v1/infra/app/db/restart"), 403);
    requester.share_lease(&lease.id, None).unwrap();
    assert!(other.shared().unwrap().is_empty());
    assert!(matches!(
        rt.block_on(theirs.open_shared(&lease.id, 5173)),
        Err(ClientError::Status(403, _))
    ));
    // Unshared, it is not viewed any more once the node asks again.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(read(&theirs, "/v1/stack"), 403);

    // One lease at a time per lane: its data is one.
    let twice = requester
        .lease(&LeaseRequest {
            node: "node-pc".into(),
            repo: "shop".into(),
            lane: "login-fix".into(),
            need: Need {
                cpus: 0.1,
                memory: 64 << 20,
                ..Need::default()
            },
            need_from: None,
            floor: None,
            profile: None,
            devstack: None,
        })
        .unwrap();
    assert!(matches!(
        rt.block_on(client.admit(&twice)),
        Err(ClientError::Status(409, _))
    ));
    let slots = rt.block_on(client.slots()).unwrap();
    assert_eq!(slots.len(), 1);
    assert!(slots[0].active && slots[0].lane == "login-fix");
    assert!(
        rt.block_on(theirs.slots()).unwrap().is_empty(),
        "nobody else's"
    );
    assert!(matches!(
        rt.block_on(client.purge_slot("shop", "login-fix")),
        Err(ClientError::Status(409, _))
    ));

    // The end of the lease keeps its slot: the mirror stays.
    rt.block_on(client.release(&lease.id)).unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while rt
        .block_on(client.slots())
        .unwrap()
        .first()
        .is_some_and(|s| s.active)
        && std::time::Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        mirror.join("src/Api/Program.cs").is_file(),
        "kept for the next lease"
    );

    // The same lane leased again sends and writes nothing that did not
    // change (#99, ADR-0038).
    let again = requester
        .lease(&LeaseRequest {
            node: "node-pc".into(),
            repo: "shop".into(),
            lane: "login-fix".into(),
            need: Need {
                cpus: 0.1,
                memory: 64 << 20,
                ..Need::default()
            },
            need_from: None,
            floor: None,
            profile: None,
            devstack: None,
        })
        .unwrap();
    let lease2 = rt.block_on(client.admit(&again)).unwrap();
    assert_eq!(lease2.slot.as_deref(), Some(slot.as_str()));
    assert!(lease.kept.is_none(), "the first lease starts fresh");
    assert!(lease2.kept.is_some(), "the next says what it starts from");
    let s4 = rt
        .block_on(client.sync(&lease2.id, tree.path(), &files(tree.path())))
        .unwrap();
    assert_eq!((s4.files, s4.uploaded, s4.written), (2, 0, 0), "{s4:?}");
    rt.block_on(client.release(&lease2.id)).unwrap();

    // Its owner throws it away: the slot and its mirror go (once the end
    // of the lease is through).
    rt.block_on(client.purge_slot("shop", "login-fix")).unwrap();
    assert!(rt.block_on(client.slots()).unwrap().is_empty());
    assert!(!mirror.exists(), "purged with its slot");
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
    // What an update an admin orders asks of the agent (ADR-0039): here it
    // runs that release already.
    let ordered = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = ordered.clone();
    let config = NodeConfig {
        on_update: Some(std::sync::Arc::new(
            move |o: &devstack_node::coordinator::UpdateOrder| {
                seen.lock().unwrap().push(o.version.clone());
                Ok(None)
            },
        )),
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
            need: Need {
                cpus: 0.1,
                memory: 64 << 20,
                ..Need::default()
            },
            need_from: None,
            floor: None,
            profile: None,
            devstack: None,
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

    // An admin has the server update its devstack (ADR-0039): ordered at
    // its next report, carried out, and over once it says so. Not for a
    // machine that is no node.
    use devstack_node::coordinator::UpdateRequest;
    let req = UpdateRequest {
        version: Some("0.99.0".into()),
        ..Default::default()
    };
    assert!(admin.admin_update(&admin_id, &req).is_err(), "no node");
    let bad = UpdateRequest {
        version: Some("latest; rm".into()),
        ..Default::default()
    };
    assert!(admin.admin_update(&node_id, &bad).is_err(), "no release");
    admin.admin_update(&node_id, &req).unwrap();
    wait_for(40, "the node carries the update out", || {
        admin.nodes().unwrap().iter().any(|n| {
            n.id == node_id
                && n.report
                    .as_ref()
                    .and_then(|r| r.update.as_ref())
                    .is_some_and(|u| u.state == "done")
        })
    });
    assert_eq!(
        *ordered.lock().unwrap(),
        [Some("0.99.0".to_string())],
        "once"
    );
    admin.admin_prune(&node_id).unwrap();
    let log: Vec<String> = admin
        .admin_audit()
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(&log[..2], ["prune", "update"]);
    stop();
}
