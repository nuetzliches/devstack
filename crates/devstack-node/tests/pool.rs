//! A machine joins a pool, is approved, talks mutual TLS, and is refused once
//! removed (ADR-0031 point 2).

use devstack_node::coordinator::{self, Report};
use devstack_node::member::Local;
use devstack_node::store::{State, Store};
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
        .approve(&m.id, &coordinator::ca(coord.path()).unwrap())
        .unwrap();
    assert_eq!(me.refresh().unwrap().state, State::Approved);
    assert_eq!(me.me().unwrap().id, m.id);

    me.report(&Report {
        sharing: true,
        cpus: 4.0,
        memory: 8 << 30,
        api: None,
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

    store.remove(&m.id).unwrap();
    let e = me.nodes().unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
}
