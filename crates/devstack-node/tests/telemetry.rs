//! Telemetry through the coordinator (ADR-0037): without a collector it is
//! refused; with one, it arrives there with the sender's identity from its
//! certificate, whatever the payload claimed, and the collector's token.

use devstack_node::coordinator;
use devstack_node::member::Local;
use devstack_node::store::{SHELL, Store};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Seen = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

/// A collector on a free loopback port that keeps what it gets: path,
/// authorization, body.
fn collector() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let s = seen.clone();
    let app = axum::Router::new().fallback(
        move |uri: axum::http::Uri,
              headers: axum::http::HeaderMap,
              axum::Json(body): axum::Json<serde_json::Value>| {
            let s = s.clone();
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                s.lock().unwrap().push((uri.path().to_string(), auth, body));
                axum::http::StatusCode::OK
            }
        },
    );
    std::thread::spawn(move || rt.block_on(async { axum::serve(listener, app).await.unwrap() }));
    (url, seen)
}

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

fn member(coord: &Path, url: &str, fp: &str, dir: &Path, name: &str) -> Local {
    let l = Local::new(dir);
    let m = l.join(url, fp, name).unwrap();
    Store::new(coord)
        .approve(&m.id, &coordinator::ca(coord).unwrap(), None, SHELL)
        .unwrap();
    l.refresh().unwrap();
    l
}

fn body() -> serde_json::Value {
    serde_json::json!({ "resourceLogs": [{
        "resource": { "attributes": [
            { "key": "service.name", "value": { "stringValue": "devstack-hub" } },
            { "key": "devstack.member.id", "value": { "stringValue": "someone-else" } },
        ]},
        "scopeLogs": [],
    }]})
}

#[test]
fn telemetry_goes_through_the_coordinator_with_the_senders_identity() {
    // No collector: the pool collects nothing, and says so.
    let c0 = tempfile::tempdir().unwrap();
    let (url0, fp0) = coordinator(c0.path());
    let m0 = tempfile::tempdir().unwrap();
    let off = member(c0.path(), &url0, &fp0, m0.path(), "dev-pc");
    assert!(!off.telemetry_info().unwrap().enabled);
    assert!(off.telemetry_send("logs", &body()).is_err());

    // A collector, configured on the coordinator only.
    let (curl, seen) = collector();
    let c = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(c.path()).unwrap();
    std::fs::write(
        c.path().join("telemetry.json"),
        serde_json::json!({ "endpoint": curl, "token": "s3cret" }).to_string(),
    )
    .unwrap();
    let (url, fp) = coordinator(c.path());
    let md = tempfile::tempdir().unwrap();
    let me = member(c.path(), &url, &fp, md.path(), "dev-pc");
    let id = me.membership().unwrap().unwrap().id;
    assert!(me.telemetry_info().unwrap().enabled);
    me.telemetry_send("logs", &body()).unwrap();

    let mut got = None;
    for _ in 0..50 {
        if let Some(x) = seen.lock().unwrap().first().cloned() {
            got = Some(x);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let (path, auth, b) = got.expect("the collector got it");
    assert_eq!(path, "/v1/logs");
    assert_eq!(auth, "Bearer s3cret");
    let attrs = b["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap()
        .clone();
    let value = |k: &str| -> Vec<String> {
        attrs
            .iter()
            .filter(|a| a["key"] == k)
            .map(|a| a["value"]["stringValue"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        value("devstack.member.id"),
        [id],
        "not what the payload claimed"
    );
    assert_eq!(value("devstack.member.name"), ["dev-pc"]);
    assert_eq!(value("service.name"), ["devstack-hub"]);
}
