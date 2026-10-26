//! A machine joins a pool, is approved, talks mutual TLS, and is refused once
//! removed (ADR-0031 point 2).

use devstack_node::coordinator::{self, Report};
use devstack_node::member::Local;
use devstack_node::store::{Role, SHELL, State, Store};
use std::path::Path;

/// A coordinator in `dir`, serving on a free loopback port until the test ends.
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

#[test]
fn a_member_joins_is_approved_and_removed() {
    let coord = tempfile::tempdir().unwrap();
    let (url, fp) = start(coord.path());
    let pc = tempfile::tempdir().unwrap();
    let me = Local::new(pc.path());

    // A wrong fingerprint is not joined.
    let wrong = "00".repeat(32);
    let e = me.join(&url, &wrong, "pc").unwrap_err().to_string();
    assert!(e.contains("not joining"), "{e}");

    let m = me.join(&url, &fp.to_uppercase(), "pc").unwrap();
    assert_eq!(m.state, State::Pending);
    assert!(me.nodes().unwrap_err().to_string().contains("not approved"));
    // Asking again is the same request.
    assert_eq!(me.join(&url, &fp, "pc").unwrap().id, m.id);

    let store = Store::new(coord.path());
    store
        .approve(&m.id, &coordinator::ca(coord.path()).unwrap(), None, SHELL)
        .unwrap();
    assert_eq!(me.refresh().unwrap().state, State::Approved);
    assert_eq!(me.me().unwrap().id, m.id);

    me.report(&Report {
        sharing: true,
        cpus: 4.0,
        memory: 8 << 30,
        api: None,
        dedicated: false,
        leases: Vec::new(),
        ..Report::default()
    })
    .unwrap();
    let nodes = me.nodes().unwrap();
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].online);
    assert_eq!(nodes[0].report.as_ref().unwrap().cpus, 4.0);

    // Without a certificate, a member's call is refused.
    let ca = std::fs::read(coord.path().join("ca.pem")).unwrap();
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::new_with_certs(&[
            ureq::tls::Certificate::from_pem(&ca).unwrap(),
        ]))
        .build();
    let anonymous = ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let r = anonymous.get(&format!("{url}/v1/nodes")).call().unwrap();
    assert_eq!(r.status(), 401);

    store.remove(&m.id, SHELL).unwrap();
    let e = me.nodes().unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
}

/// Admins approve, reject, remove and change roles over the API; members may
/// not (ADR-0032).
#[test]
fn an_admin_manages_the_pool_over_the_api() {
    let coord = tempfile::tempdir().unwrap();
    let (url, fp) = start(coord.path());
    let store = Store::new(coord.path());
    let ca = coordinator::ca(coord.path()).unwrap();

    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let [admin, colleague, stranger] = [0, 1, 2].map(|i| Local::new(dirs[i].path()));
    let a = admin.join(&url, &fp, "admin-pc").unwrap();
    store.approve(&a.id, &ca, Some(Role::Admin), SHELL).unwrap();
    admin.refresh().unwrap();
    assert_eq!(admin.me().unwrap().role, Role::Admin);

    let c = colleague.join(&url, &fp, "colleague-pc").unwrap();
    let s = stranger.join(&url, &fp, "stranger").unwrap();
    let pending = admin.admin_members().unwrap();
    let asked = pending.iter().find(|m| m.id == c.id).unwrap();
    assert_eq!(asked.state, State::Pending);
    assert_eq!(
        asked.address.as_deref(),
        Some("127.0.0.1"),
        "admins see where a request came from"
    );

    admin.admin_approve(&c.id, None).unwrap();
    admin.admin_remove(&s.id).unwrap();
    colleague.refresh().unwrap();
    assert_eq!(stranger.refresh().unwrap().state, State::Removed);

    // A member is no admin.
    let e = colleague.admin_members().unwrap_err().to_string();
    assert!(e.starts_with("403"), "{e}");
    assert!(
        colleague
            .members()
            .unwrap()
            .iter()
            .all(|m| m.address.is_none())
    );

    // The last admin stays one; with a second, the first may step down.
    let e = admin
        .admin_role(&a.id, Role::Member)
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("409"), "{e}");
    admin.admin_role(&c.id, Role::Admin).unwrap();
    admin.admin_role(&a.id, Role::Member).unwrap();
    assert!(admin.admin_members().is_err());

    let log: Vec<String> = colleague
        .admin_audit()
        .unwrap()
        .into_iter()
        .map(|e| format!("{} {}", e.action, e.name))
        .collect();
    assert_eq!(
        log,
        [
            "role admin-pc",
            "role colleague-pc",
            "reject stranger",
            "approve colleague-pc",
            "approve admin-pc"
        ]
    );
}

/// Each party says which devstack it runs (ADR-0031, amended 2026-10-19):
/// the coordinator passes a node's on, refuses a node with another pool
/// protocol, places on a node that does not say, and tells whoever opens a
/// shared lane what its node and owner run.
#[test]
fn the_pool_knows_which_devstack_each_party_runs() {
    use devstack_node::coordinator::{LeaseReport, LeaseRequest, SharedService, Sharing};
    use devstack_node::share::Need;
    use devstack_node::version::{PROTOCOL, Release};

    let coord = tempfile::tempdir().unwrap();
    let (url, fp) = start(coord.path());
    let store = Store::new(coord.path());
    let ca = coordinator::ca(coord.path()).unwrap();
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let [node, owner, viewer] = [0, 1, 2].map(|i| Local::new(dirs[i].path()));
    let mut ids = Vec::new();
    for (l, name) in [
        (&node, "build-box"),
        (&owner, "owner-pc"),
        (&viewer, "viewer-pc"),
    ] {
        let m = l.join(&url, &fp, name).unwrap();
        store.approve(&m.id, &ca, None, SHELL).unwrap();
        l.refresh().unwrap();
        ids.push(m.id);
    }
    store.set_node(&ids[0], true, SHELL).unwrap();

    let node_report = |devstack: Option<Release>, leases: Vec<LeaseReport>| {
        node.report(&Report {
            sharing: true,
            cpus: 4.0,
            memory: 8 << 30,
            api: Some("https://127.0.0.1:1".into()),
            devstack,
            leases,
            ..Report::default()
        })
        .unwrap()
    };
    let ask = |node: &str, devstack: Option<Release>| {
        owner.lease(&LeaseRequest {
            node: node.into(),
            repo: "shop".into(),
            lane: "fix".into(),
            need: Need {
                cpus: 0.5,
                memory: 1 << 30,
                ..Need::default()
            },
            need_from: None,
            floor: None,
            profile: None,
            devstack,
            warm: false,
        })
    };
    let newer = Release {
        version: "99.0.0".into(),
        protocol: PROTOCOL + 1,
    };

    // The coordinator says what it runs in its answers.
    let answer = node_report(Some(newer.clone()), Vec::new());
    assert_eq!(answer.coordinator, Some(Release::this()));

    // Another protocol: refused by name, not placed by the pool.
    let e = ask("build-box", Some(Release::this()))
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("409"), "{e}");
    assert!(
        e.contains("99.0.0") && e.contains(&Release::this().version),
        "both releases named: {e}"
    );
    let e = ask("pool", Some(Release::this())).unwrap_err().to_string();
    assert!(e.starts_with("409"), "{e}");
    // A requester that does not say is placed: it is older, and warns.
    let offer = ask("build-box", None).unwrap();
    assert_eq!(offer.node.devstack, Some(newer));

    // A node that does not say (older) is placed, and known as unknown.
    node_report(None, Vec::new());
    let offer = ask("pool", Some(Release::this())).unwrap();
    assert_eq!(offer.node.devstack, None);
    assert_eq!(offer.coordinator, Some(Release::this()));

    // The same release: passed on.
    node_report(Some(Release::this()), Vec::new());
    let offer = ask("build-box", Some(Release::this())).unwrap();
    assert_eq!(offer.node.devstack, Some(Release::this()));
    let nodes = viewer.nodes().unwrap();
    let n = nodes.iter().find(|n| n.id == ids[0]).unwrap();
    assert_eq!(
        n.report.as_ref().unwrap().devstack,
        Some(Release::this()),
        "the pool area shows each node's release"
    );

    // Shared: whoever opens it sees the node's and the owner's release.
    owner
        .report(&Report {
            devstack: Some(Release::this()),
            ..Report::default()
        })
        .unwrap();
    node_report(
        Some(Release::this()),
        vec![LeaseReport {
            id: offer.id.clone(),
            member: ids[1].clone(),
            repo: "shop".into(),
            lane: "fix".into(),
            expires_in: 100,
            need: None,
        }],
    );
    owner
        .share_lease(
            &offer.id,
            Some(&Sharing::new(
                vec!["*".into()],
                vec![SharedService {
                    name: "web".into(),
                    port: 8080,
                    scheme: "http".into(),
                    here: 8180,
                }],
            )),
        )
        .unwrap();
    let shared = viewer.shared().unwrap();
    let l = shared.iter().find(|l| l.lease == offer.id).unwrap();
    assert_eq!(l.node.devstack, Some(Release::this()));
    assert_eq!(l.owner_devstack, Some(Release::this()));
}
