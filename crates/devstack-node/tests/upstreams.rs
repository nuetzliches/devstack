//! The MCP upstreams a pool names (ADR-0069 option B): any member reads the
//! list, only an admin changes it, every change is in the audit log, and an
//! entry is a URL over https, never a command.

use devstack_node::coordinator;
use devstack_node::member::{Local, MemberError};
use devstack_node::store::{Role, SHELL, Store};
use devstack_node::upstreams::{PoolAuth, PoolUpstream};
use std::path::Path;

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

fn wiki(url: &str) -> PoolUpstream {
    PoolUpstream {
        name: "wiki".into(),
        url: url.into(),
        auth: PoolAuth::Oauth {
            client_id: None,
            scopes: Vec::new(),
        },
        ..Default::default()
    }
}

#[test]
fn members_read_the_pools_upstreams_and_only_an_admin_changes_them() {
    let c = tempfile::tempdir().unwrap();
    let (url, fp) = coordinator(c.path());
    let ad = tempfile::tempdir().unwrap();
    let admin = member(c.path(), &url, &fp, ad.path(), "lead-pc", Role::Admin);
    let md = tempfile::tempdir().unwrap();
    let dev = member(c.path(), &url, &fp, md.path(), "dev-pc", Role::Member);

    assert!(dev.upstreams().unwrap().is_empty());
    let refused = dev.admin_upstream_put(&wiki("https://wiki.example.com/mcp"));
    assert!(
        matches!(refused, Err(MemberError::Status(403, _))),
        "{refused:?}"
    );

    admin
        .admin_upstream_put(&wiki("https://wiki.example.com/mcp"))
        .unwrap();
    assert_eq!(
        dev.upstreams().unwrap(),
        [wiki("https://wiki.example.com/mcp")]
    );

    // Never the member's own machine, never plain http.
    let local = admin.admin_upstream_put(&wiki("http://127.0.0.1:9000/mcp"));
    assert!(
        matches!(local, Err(MemberError::Status(400, _))),
        "{local:?}"
    );

    assert!(matches!(
        dev.admin_upstream_remove("wiki"),
        Err(MemberError::Status(403, _))
    ));
    admin.admin_upstream_remove("wiki").unwrap();
    assert!(dev.upstreams().unwrap().is_empty());
    assert!(matches!(
        admin.admin_upstream_remove("wiki"),
        Err(MemberError::Status(404, _))
    ));

    let audit = Store::new(c.path()).audit(50).unwrap();
    let actions: Vec<&str> = audit
        .iter()
        .filter(|e| e.action.starts_with("upstream"))
        .map(|e| e.action.as_str())
        .collect();
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert!(actions.contains(&"upstream") && actions.contains(&"upstream-remove"));
    assert!(
        audit
            .iter()
            .filter(|e| e.action.starts_with("upstream"))
            .all(|e| e.by.contains("lead-pc"))
    );
}
