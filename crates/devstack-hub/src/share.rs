//! This machine in the pool (ADR-0031): the hub measures what the machine has
//! and what is free and reports it to the coordinator every 15 s, which also
//! tells the pool the machine is online and which devstack it runs. A machine
//! in no pool, or not approved yet, reports nothing. Nor does the hub for a
//! pool while a node agent of the same membership runs here: the agent
//! reports the machine then, with its API and its leases, which a report of
//! the hub's would drop (#170).
//!
//! The hub offers nothing to the pool. A machine runs other members' lanes
//! with a node agent (`devstack node agent`, granted by an admin), and the
//! dashboard's share switch never started one: it only reported capacity
//! nobody could use, and is gone until it does (#171; on Windows, which
//! cannot run the Linux lanes, #168).
//!
//! - `GET /hub/pool/share?pool=`: what is measured, the last report, the
//!   coordinator's devstack, and why nothing is offered (`unavailable`)

use axum::Json;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::coordinator::Report;
use devstack_node::share::{Amount, idle_cpus};
use devstack_node::store::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(15);

/// Why the hub offers nothing to the pool.
const NOT_SHARED: &str = "A machine runs other members' lanes with a node agent \
     (`devstack node agent`, granted by an admin); the dashboard does not start one yet \
     (#171, on Windows #168).";

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// What the machine has in all.
    pub total: Amount,
    /// What is free now.
    pub free: Amount,
    /// Whether it belongs to a pool as an approved member (else nothing is
    /// reported).
    pub member: bool,
    /// When the last report reached the coordinator (Unix ms).
    pub reported_at: Option<u128>,
    /// Why the last report failed.
    pub error: Option<String>,
    /// A node agent of this membership runs here and reports the machine;
    /// the hub does not (#170).
    pub agent: bool,
    /// The devstack the coordinator runs, from its answer to the last
    /// report; `None` before one, or from an older coordinator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<devstack_node::version::Release>,
    /// Why this machine offers the pool nothing.
    pub unavailable: &'static str,
}

#[derive(Deserialize, Default)]
pub struct PoolQuery {
    #[serde(default)]
    pool: Option<String>,
}

impl PoolQuery {
    fn name(&self) -> Option<String> {
        let p = self
            .pool
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(devstack_node::member::DEFAULT_POOL);
        devstack_node::member::valid_pool_name(p).then(|| p.to_string())
    }
}

/// Per pool.
static STATUS: OnceLock<Mutex<BTreeMap<String, Status>>> = OnceLock::new();

fn state() -> std::path::PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

fn dir(pool: &str) -> std::path::PathBuf {
    devstack_node::member::pool_dir(&state(), pool)
}

fn entry<'a>(map: &'a mut BTreeMap<String, Status>, pool: &str) -> &'a mut Status {
    map.entry(pool.to_string()).or_insert_with(|| Status {
        unavailable: NOT_SHARED,
        ..Status::default()
    })
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Start measuring and reporting, once per hub.
pub fn start() {
    if STATUS.set(Mutex::new(BTreeMap::new())).is_err() {
        return;
    }
    std::thread::spawn(run);
}

fn run() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut before = devstack_proc::cpu_times();
    // The first CPU reading needs a second one to say anything.
    let mut wait = Duration::from_secs(2);
    loop {
        std::thread::sleep(wait);
        wait = EVERY;
        let after = devstack_proc::cpu_times();
        let free = Amount {
            cpus: match (before, after) {
                (Some(b), Some(a)) => idle_cpus(b, a, cores),
                _ => 0.0,
            },
            memory: devstack_proc::memory_available().unwrap_or(0),
        };
        if after.is_some() {
            before = after;
        }
        let total = Amount {
            cpus: cores as f64,
            memory: devstack_proc::memory_total().unwrap_or(0),
        };
        let Some(status) = STATUS.get() else { return };
        let gpus = devstack_node::share::gpus();
        for (pool, _) in devstack_node::member::pools(&state()) {
            let result = round(&dir(&pool), total, &gpus);
            let mut map = status.lock().unwrap();
            let st = entry(&mut map, &pool);
            st.total = total;
            st.free = free;
            st.agent = matches!(result, Ok(Round::Agent));
            match result {
                Ok(Round::NoMember) => {
                    st.member = false;
                    st.error = None;
                }
                Ok(Round::Agent) => {
                    st.member = true;
                    st.error = None;
                }
                Ok(Round::Reported(coordinator)) => {
                    st.member = true;
                    st.error = None;
                    st.reported_at = Some(now_ms());
                    st.coordinator = coordinator;
                }
                Err(e) => st.error = Some(e),
            }
            drop(map);
            crate::guests::seen(&pool, guests(&pool));
        }
    }
}

/// Other members' lanes on this machine in `pool`; `None` when the
/// coordinator could not be asked. The member list is read only when a
/// lease names this machine.
fn guests(pool: &str) -> Option<Vec<crate::guests::Guest>> {
    let local = devstack_node::member::Local::new(&dir(pool));
    let m = match local.membership() {
        Ok(Some(m)) if m.state == State::Approved => m,
        Ok(_) => return Some(Vec::new()),
        Err(_) => return None,
    };
    let leases = local.leases().ok()?;
    if !leases.iter().any(|l| l.node == m.id && l.member != m.id) {
        return Some(Vec::new());
    }
    let nodes = local.nodes().ok()?;
    Some(crate::guests::of(pool, &m.id, &leases, &nodes))
}

/// What one round did for a pool.
#[derive(Debug, PartialEq)]
enum Round {
    /// This machine is no approved member: nothing to report to.
    NoMember,
    /// A node agent of the membership runs here and reports (#170).
    Agent,
    /// Reported; what the coordinator says it runs.
    Reported(Option<devstack_node::version::Release>),
}

/// Report the membership in `dir` to the coordinator, offering nothing,
/// unless a node agent of it runs here. Its report carries no API and no
/// leases: sent while the agent runs, the coordinator would take the node
/// for one without an agent and drop its leases until the agent's next
/// report, which brings them back new, without their sharing.
fn round(
    dir: &std::path::Path,
    total: Amount,
    gpus: &[devstack_node::share::Gpu],
) -> Result<Round, String> {
    let local = devstack_node::member::Local::new(dir);
    match local.membership() {
        Ok(Some(m)) if m.state == State::Approved => {}
        Ok(_) => return Ok(Round::NoMember),
        Err(e) => return Err(e.to_string()),
    }
    if devstack_node::node::agent_runs(dir) {
        return Ok(Round::Agent);
    }
    local
        .report(&Report {
            sharing: false,
            cpus: 0.0,
            memory: 0,
            api: None,
            dedicated: false,
            devstack: Some(devstack_node::version::Release::this()),
            kept: None,
            update: None,
            no_update: None,
            slots: Vec::new(),
            disk_free: None,
            disk_room: None,
            runs: Vec::new(),
            leases: Vec::new(),
            total: Some(total),
            used: None,
            gpus: gpus
                .iter()
                .map(|g| devstack_node::share::Gpu {
                    offered: false,
                    ..g.clone()
                })
                .collect(),
        })
        .map(|a| Round::Reported(a.coordinator))
        .map_err(|e| e.to_string())
}

pub async fn get(Query(q): Query<PoolQuery>) -> Response {
    let Some(pool) = q.name() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "pool: a-z, 0-9, `-` and `_`, at most 32" })),
        )
            .into_response();
    };
    match STATUS.get() {
        Some(status) => {
            let mut map = status.lock().unwrap();
            // A pool not reported yet still shows what is measured.
            let any = map.values().next().cloned();
            let st = entry(&mut map, &pool);
            if let Some(a) = any.filter(|_| st.reported_at.is_none() && st.total.cpus == 0.0) {
                st.total = a.total;
                st.free = a.free;
            }
            Json(st.clone()).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "reporting has not started" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Round, round};
    use devstack_node::coordinator::{self, LeaseRequest, SharedService, Sharing};
    use devstack_node::member::Local;
    use devstack_node::node::{self, NodeConfig};
    use devstack_node::nodeclient::NodeClient;
    use devstack_node::share::{Amount, Need, Settings};
    use devstack_node::store::{SHELL, Store};
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    fn wait_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(secs);
        while !f() {
            assert!(Instant::now() < until, "{what}");
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

    /// A machine that runs the hub and a node agent of the same membership
    /// (#170): while the agent runs, the hub does not report, so the node
    /// keeps its API, and a running lease its `created` and its sharing,
    /// across the hub's rounds. Once the agent is gone, the hub reports.
    #[test]
    fn the_hub_does_not_report_over_a_node_agent_of_the_same_member() {
        let c = tempfile::tempdir().unwrap();
        let fp = coordinator::init(c.path(), &["127.0.0.1".into()]).unwrap();
        let rt_coord = rt();
        let listener = rt_coord
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .unwrap();
        let url = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        let cdir = c.path().to_path_buf();
        std::thread::spawn(move || {
            rt_coord
                .block_on(coordinator::serve(&cdir, listener, std::future::pending()))
                .unwrap();
        });
        let (md, rd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let machine = member(c.path(), &url, &fp, md.path(), "dev-box");
        let requester = member(c.path(), &url, &fp, rd.path(), "laptop");
        let id = machine.membership().unwrap().unwrap().id;
        Store::new(c.path()).set_node(&id, true, SHELL).unwrap();
        let total = Amount {
            cpus: 8.0,
            memory: 16 << 30,
        };

        // No agent yet: the hub reports the machine.
        assert!(matches!(
            round(md.path(), total, &[]),
            Ok(Round::Reported(_))
        ));

        // The agent of the same membership, until `stop`.
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
            dir: md.path().to_path_buf(),
            api: api.clone(),
            share: Settings {
                enabled: true,
                max_cpus: Some(4.0),
                max_memory: Some(4 << 30),
            },
            dedicated: false,
            // Fixed, so that a busy machine running the test does not
            // shrink the offer below what the lane needs.
            free: Some(Amount {
                cpus: 8.0,
                memory: 8 << 30,
            }),
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let agent = std::thread::spawn(move || {
            rt_node
                .block_on(node::serve(config, listener, async {
                    let _ = stopped.await;
                }))
                .unwrap();
        });
        let node_api = || {
            requester
                .nodes()
                .unwrap()
                .into_iter()
                .find(|n| n.id == id)
                .and_then(|n| n.report.and_then(|r| r.api))
        };
        wait_for(20, "the agent reports its API", || {
            node_api().as_deref() == Some(api.as_str())
        });

        // A running lease on it, shared.
        let offer = requester
            .lease(&LeaseRequest {
                node: "dev-box".into(),
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
            .unwrap();
        let rt = rt();
        let client = NodeClient::new(&requester, offer.node.clone()).unwrap();
        rt.block_on(client.admit(&offer)).unwrap();
        let sharing = Sharing::new(
            vec!["*".into()],
            vec![SharedService {
                name: "web".into(),
                port: 5173,
                scheme: "http".into(),
                here: 5273,
            }],
        );
        requester.share_lease(&offer.id, Some(&sharing)).unwrap();
        let lease = || {
            requester
                .leases()
                .unwrap()
                .into_iter()
                .find(|l| l.node == id && l.lane == "fix")
        };
        wait_for(20, "the agent reports the lease running", || {
            lease().is_some_and(|l| l.state == "running")
        });
        let before = lease().unwrap();
        assert_eq!(before.shared.as_ref(), Some(&sharing));
        // A lease the coordinator took for new would have a later `created`.
        std::thread::sleep(Duration::from_millis(1100));

        // The hub's rounds leave the node to its agent.
        for _ in 0..3 {
            assert_eq!(round(md.path(), total, &[]), Ok(Round::Agent));
            assert_eq!(node_api().as_deref(), Some(api.as_str()), "the API stays");
            let now = lease().expect("the lease stays");
            assert_eq!(
                (now.id.as_str(), now.created, now.shared.as_ref()),
                (before.id.as_str(), before.created, Some(&sharing))
            );
            std::thread::sleep(Duration::from_millis(300));
        }

        // The agent stops: the hub reports the machine again.
        let _ = stop.send(());
        agent.join().unwrap();
        assert!(!node::agent_runs(md.path()));
        assert!(matches!(
            round(md.path(), total, &[]),
            Ok(Round::Reported(_))
        ));
        assert_eq!(node_api(), None, "a hub's report has no API");
    }
}
