//! A lease the pool cannot place only because lanes are starting on a node
//! waits for them to settle rather than be refused (#524): offered once the
//! node takes it, in the order the leases came; refused as before when no
//! node would take it even then, and for a requester that does not wait.

use devstack_node::coordinator::{self, LeaseRequest, Leasing, Report, Settling};
use devstack_node::member::Local;
use devstack_node::share::Need;
use devstack_node::store::{SHELL, Store};
use std::path::Path;

fn start(dir: &Path) -> (String, String) {
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

fn ask(lane: &str) -> LeaseRequest {
    LeaseRequest {
        node: "pool".into(),
        repo: "shop".into(),
        lane: lane.into(),
        need: Need {
            cpus: 2.0,
            memory: 2 << 30,
            ..Need::default()
        },
        need_from: None,
        floor: None,
        profile: None,
        devstack: None,
        warm: false,
    }
}

fn waiting(l: Leasing) -> coordinator::Provisioning {
    match l {
        Leasing::Provisioning(p) => p,
        Leasing::Offer(o) => panic!("offered: {o:?}"),
    }
}

#[test]
fn a_lease_waits_for_the_lanes_starting_on_a_node_to_settle() {
    let coord = tempfile::tempdir().unwrap();
    let (url, fp) = start(coord.path());
    let store = Store::new(coord.path());
    let ca = coordinator::ca(coord.path()).unwrap();
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let [node, dev, other] = [0, 1, 2].map(|i| Local::new(dirs[i].path()));
    let mut ids = Vec::new();
    for (l, name) in [(&node, "build-box"), (&dev, "dev-pc"), (&other, "other-pc")] {
        let m = l.join(&url, &fp, name).unwrap();
        store.approve(&m.id, &ca, None, SHELL).unwrap();
        l.refresh().unwrap();
        ids.push(m.id);
    }
    store.set_node(&ids[0], true, SHELL).unwrap();
    let report = |cpus: f64, settling: Option<Settling>| {
        node.report(&Report {
            sharing: true,
            cpus,
            memory: 32 << 30,
            api: Some("https://127.0.0.1:1".into()),
            dedicated: true,
            settling,
            ..Report::default()
        })
        .unwrap();
    };
    let starting = |cpus: f64| {
        Some(Settling {
            cpus,
            memory: 4 << 30,
            ends_in: 120,
        })
    };

    // A lane starting there holds the node's offer.
    report(0.5, starting(9.0));
    // A requester that does not wait is refused as before.
    let e = dev.lease(&ask("a")).unwrap_err().to_string();
    assert!(e.starts_with("409"), "{e}");
    // One that waits is told for what, and how long at most.
    let p = waiting(dev.lease_or_wait(&ask("a")).unwrap());
    assert_eq!(p.state, "settling");
    assert!(
        p.why.contains("build-box") && p.why.contains("starting"),
        "{}",
        p.why
    );
    assert_eq!(p.limit, 180);
    // A second requester waits behind it.
    let q = waiting(other.lease_or_wait(&ask("b")).unwrap());
    assert!(waiting(dev.wait(&p.id).unwrap()).why.contains("build-box"));

    // Room for one, and another start: the one that asked first goes first.
    report(2.5, starting(3.0));
    let behind = waiting(other.wait(&q.id).unwrap());
    assert!(behind.why.contains("goes first"), "{}", behind.why);
    let Leasing::Offer(o) = dev.wait(&p.id).unwrap() else {
        panic!("not offered");
    };
    assert_eq!(o.node.name, "build-box");
    assert_eq!(o.id, p.id);
    // The second waits on: the node takes it once that start settled.
    let behind = waiting(other.wait(&q.id).unwrap());
    assert!(behind.why.contains("build-box"), "{}", behind.why);

    // No node would take it even then: refused, for both.
    report(0.5, starting(0.5));
    let e = other.wait(&q.id).unwrap_err().to_string();
    assert!(
        e.starts_with("409") && e.contains("no node of the pool"),
        "{e}"
    );
    let e = dev.lease_or_wait(&ask("c")).unwrap_err().to_string();
    assert!(e.starts_with("409"), "{e}");

    // A warm lease does not wait.
    report(0.5, starting(9.0));
    let e = dev
        .lease_or_wait(&LeaseRequest {
            warm: true,
            ..ask("d")
        })
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("409"), "{e}");
}
