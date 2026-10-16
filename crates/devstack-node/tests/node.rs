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
    let rt = rt();
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
    };
    std::thread::spawn(move || {
        rt_node
            .block_on(node::serve(config, listener, std::future::pending()))
            .unwrap();
    });
    let node_id = node_member.membership().unwrap().unwrap().id;
    // Online once it has reported, with its API.
    let mut online = false;
    for _ in 0..50 {
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

    rt.block_on(client.release(&lease.id)).unwrap();
    assert!(
        !mirror.exists(),
        "a released lease takes its mirror with it"
    );
}
