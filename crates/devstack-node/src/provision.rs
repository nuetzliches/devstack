//! Nodes a provisioner makes (ADR-0051): the contract between a coordinator
//! and the HTTP service that rents machines for it, the pool's settings for
//! it, the machines the coordinator asked for, and a mock provisioner to try
//! it all without a cloud.
//!
//! The provisioner (`devstack node serve --provisioner <url>`) holds the
//! provider's credentials; the coordinator sends a bearer token
//! (`DEVSTACK_PROVISIONER_TOKEN`, else `provisioner.token` in its
//! directory) and calls:
//!
//! - `POST /machines` [`MachineRequest`] → [`MachineAnswer`]: make a machine
//!   that holds at least the lane's need, and have it run `devstack join
//!   <coordinator> --fingerprint <fp> --enrol <token>` and a node agent;
//! - `DELETE /machines/{machine}` → `2xx`, or `404` (gone already): delete
//!   it;
//! - `POST /events` [`Event`] → `2xx`: what happened, to bill from.

use crate::share::{Amount, Need};
use crate::version::Release;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// The longest a start waits for a machine (ADR-0051 point 5), whatever the
/// provisioner estimates.
pub const MAX_WAIT: u64 = 600;
/// The shortest: an estimate below it is taken as this.
pub const MIN_WAIT: u64 = 60;

/// How a pool's provisioned nodes are bound.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Per {
    /// A provisioned node runs every member's lanes (ADR-0051 point 2).
    #[default]
    Pool,
    /// It runs the lanes of the repository it was made for only.
    Repo,
}

/// Whether an admin confirms each provisioned node (ADR-0051 point 6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confirm {
    #[default]
    None,
    /// It admits no lease until an admin confirms it.
    Admin,
}

fn idle_default() -> u64 {
    30
}

fn nodes_default() -> u32 {
    2
}

/// The pool's settings for provisioning, `provisioning.json` in the
/// coordinator's directory; read at each use, so a change from the
/// coordinator's shell holds at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub per: Per,
    #[serde(default)]
    pub confirm: Confirm,
    /// How long a provisioned node without a lease, and a stopped lane's
    /// slot on it, are kept (ADR-0051 point 7).
    #[serde(default = "idle_default")]
    pub idle_minutes: u64,
    /// Provisioned nodes at once (ADR-0051 point 3).
    #[serde(default = "nodes_default")]
    pub max_nodes: u32,
    /// CPU cores and memory of all provisioned nodes together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpus: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            per: Per::Pool,
            confirm: Confirm::None,
            idle_minutes: idle_default(),
            max_nodes: nodes_default(),
            max_cpus: None,
            max_memory: None,
        }
    }
}

const SETTINGS: &str = "provisioning.json";
const MACHINES: &str = "machines.json";

impl Settings {
    /// The settings in the coordinator's `dir`; the defaults without a file.
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join(SETTINGS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        write_whole(
            &dir.join(SETTINGS),
            &serde_json::to_vec_pretty(self).expect("settings serialise"),
        )
    }
}

pub(crate) fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", crate::coordinator::random_hex(4)));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// What the coordinator asks of the provisioner: a machine for a lane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRequest {
    /// The coordinator's id of this request; the same in its events.
    pub request: String,
    /// What the lane that no node could take needs; the provisioner
    /// chooses the size, usually one that holds several such lanes.
    pub need: Need,
    /// The repository the machine is for, when the pool provisions per
    /// repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The coordinator's devstack: the machine runs one that works with it
    /// (the same pool protocol).
    pub devstack: Release,
    pub enrol: Enrolment,
}

/// How the machine joins the pool: once, from the address the provisioner
/// names for it in its answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrolment {
    /// `https://host:port`
    pub coordinator: String,
    /// The pool CA's fingerprint.
    pub fingerprint: String,
    /// Single use, bound to this request and the machine's address.
    pub token: String,
    /// Seconds it is good for.
    pub expires_in: u64,
    /// What the machine runs to join, for convenience.
    pub command: String,
}

/// The provisioner's answer: accepted, with the machine, or refused with a
/// reason.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineAnswer {
    pub accepted: bool,
    /// The provider's id of the machine (what `DELETE` names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// The public address it will have: the coordinator takes its
    /// enrolment from there only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// What it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Amount>,
    /// Seconds until it has enrolled, as the provisioner expects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u64>,
    /// Why it refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What happened to a machine (ADR-0051 point 8): `node_ready`,
/// `lease_begun`, `lease_ended`, `node_returned`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub kind: String,
    pub request: String,
    pub machine: String,
    /// Unix seconds.
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<LeaseEvent>,
}

/// A lease on a provisioned node, as its events tell it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseEvent {
    pub id: String,
    pub member: String,
    pub repo: String,
    pub lane: String,
    pub need: Need,
    /// Unix seconds.
    pub began: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<u64>,
}

/// Where a machine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    /// Asked for; not enrolled yet.
    Provisioning,
    /// Enrolled: a node of the pool.
    Ready,
    /// Being given back: deleted, and retried until the provisioner
    /// confirms it.
    Returning,
}

/// A machine the coordinator asked for, `machines.json` in its directory:
/// kept until the provisioner confirms it deleted (ADR-0051 point 7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub request: String,
    /// The provider's id; empty until the provisioner answered.
    #[serde(default)]
    pub machine: String,
    /// Where it enrols from; empty until the provisioner answered.
    #[serde(default)]
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub state: MachineState,
    /// The member whose lane caused it.
    pub requester: String,
    /// Unix seconds.
    pub created: u64,
    /// It enrols by then, or is given back.
    pub deadline: u64,
    /// The enrolment token's hash (blake3, hex); the token itself is never
    /// kept.
    #[serde(default, skip_serializing)]
    pub token_hash: String,
    /// The member it enrolled as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Since when it has had no lease (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<u64>,
    /// A second use of its token, or one from another address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarm: Option<String>,
    /// Why the last deletion failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The token hash `machines.json` keeps on disk (`token_hash` is not sent
/// to admins, so it is written here apart).
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(flatten)]
    machine: Machine,
    #[serde(default)]
    token_hash: String,
}

/// The machines in the coordinator's `dir`.
pub fn load_machines(dir: &Path) -> Vec<Machine> {
    let stored: Vec<Stored> = std::fs::read(dir.join(MACHINES))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    stored
        .into_iter()
        .map(|s| Machine {
            token_hash: s.token_hash,
            ..s.machine
        })
        .collect()
}

pub fn save_machines(dir: &Path, machines: &[Machine]) -> std::io::Result<()> {
    let stored: Vec<Stored> = machines
        .iter()
        .map(|m| Stored {
            token_hash: m.token_hash.clone(),
            machine: m.clone(),
        })
        .collect();
    let bytes = serde_json::to_vec_pretty(&stored).expect("machines serialise");
    write_whole(&dir.join(MACHINES), &bytes)
}

/// A token's hash, as `Machine::token_hash` keeps it.
pub fn token_hash(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

/// How long a start waits for a machine the provisioner estimates `est`
/// seconds for.
pub fn wait_for(est: Option<u64>) -> u64 {
    est.unwrap_or(MAX_WAIT).clamp(MIN_WAIT, MAX_WAIT)
}

/// Whether one more machine for `need` stays within the caps, given the
/// machines there are; `Err` says which cap holds.
pub fn within_caps(s: &Settings, machines: &[Machine], need: &Need) -> Result<(), String> {
    let count = machines.len() as u32;
    if count >= s.max_nodes {
        return Err(format!(
            "the pool has {count} provisioned node{} of at most {}",
            if count == 1 { "" } else { "s" },
            s.max_nodes
        ));
    }
    let (cpus, memory) = machines.iter().fold((0.0, 0u64), |(c, m), x| {
        let a = x.size.unwrap_or(Amount {
            cpus: 0.0,
            memory: 0,
        });
        (c + a.cpus, m.saturating_add(a.memory))
    });
    if let Some(max) = s.max_cpus
        && cpus + need.cpus > max
    {
        return Err(format!(
            "provisioned nodes hold {cpus:.1} CPU, at most {max:.1} together"
        ));
    }
    if let Some(max) = s.max_memory
        && memory.saturating_add(need.memory) > max
    {
        return Err(format!(
            "provisioned nodes hold {}, at most {} together",
            crate::coordinator::gib(memory),
            crate::coordinator::gib(max)
        ));
    }
    Ok(())
}

/// The provisioner as the coordinator reaches it.
#[derive(Debug, Clone)]
pub struct Provisioner {
    pub url: String,
    pub token: Option<String>,
}

impl Provisioner {
    /// The provisioner at `url`, with the token from
    /// `DEVSTACK_PROVISIONER_TOKEN`, else `provisioner.token` in `dir`.
    pub fn new(dir: &Path, url: &str) -> Self {
        let token = std::env::var("DEVSTACK_PROVISIONER_TOKEN")
            .ok()
            .or_else(|| std::fs::read_to_string(dir.join("provisioner.token")).ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        Self {
            url: url.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .new_agent()
    }

    fn auth<B>(&self, r: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => r.header("Authorization", format!("Bearer {t}")),
            None => r,
        }
    }

    fn answer(
        r: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<(u16, String), String> {
        let mut r = r.map_err(|e| format!("the provisioner: {e}"))?;
        let status = r.status().as_u16();
        let text = r
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("the provisioner: {e}"))?;
        Ok((status, text))
    }

    /// Ask for a machine.
    pub fn request(&self, r: &MachineRequest) -> Result<MachineAnswer, String> {
        let (status, text) = Self::answer(
            self.auth(Self::agent().post(&format!("{}/machines", self.url)))
                .header("Content-Type", "application/json")
                .send(serde_json::to_string(r).expect("a request serialises")),
        )?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "the provisioner answered {status}: {}",
                text.trim()
            ));
        }
        serde_json::from_str(&text).map_err(|e| format!("the provisioner's answer: {e}"))
    }

    /// Delete a machine; gone already is deleted.
    pub fn delete(&self, machine: &str) -> Result<(), String> {
        let (status, text) = Self::answer(
            self.auth(Self::agent().delete(&format!("{}/machines/{machine}", self.url)))
                .call(),
        )?;
        if (200..300).contains(&status) || status == 404 {
            Ok(())
        } else {
            Err(format!(
                "the provisioner answered {status}: {}",
                text.trim()
            ))
        }
    }

    /// Tell what happened.
    pub fn event(&self, e: &Event) -> Result<(), String> {
        let (status, text) = Self::answer(
            self.auth(Self::agent().post(&format!("{}/events", self.url)))
                .header("Content-Type", "application/json")
                .send(serde_json::to_string(e).expect("an event serialises")),
        )?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!(
                "the provisioner answered {status}: {}",
                text.trim()
            ))
        }
    }
}

pub mod mock {
    //! A provisioner without a cloud, for tests and for trying provisioning
    //! on one machine: it "makes" a machine by answering with a size and an
    //! address and handing the request to a hook (which may enrol a member
    //! from there, or run a script), records every deletion and event, and
    //! can refuse, fail deletions, or name another address.

    use super::*;
    use axum::extract::{Path as UrlPath, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{delete, post};
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex};

    /// What the mock answers.
    #[derive(Debug, Clone)]
    pub struct Behaviour {
        /// The size of each machine it makes.
        pub size: Amount,
        /// The address it names for each machine.
        pub address: String,
        pub estimate: u64,
        /// Refuse every request with this reason.
        pub refuse: Option<String>,
        /// Fail this many deletions (each answered 500) before one succeeds.
        pub fail_deletes: u32,
        /// The bearer token it expects; `None`: any.
        pub token: Option<String>,
        /// How long after answering it hands a request to the hook.
        pub hook_after: Duration,
    }

    impl Default for Behaviour {
        fn default() -> Self {
            Self {
                size: Amount {
                    cpus: 8.0,
                    memory: 16 << 30,
                },
                address: "127.0.0.1".into(),
                estimate: 60,
                refuse: None,
                fail_deletes: 0,
                token: None,
                hook_after: Duration::from_millis(200),
            }
        }
    }

    /// What the mock was asked, and what it did.
    #[derive(Debug, Clone, Default)]
    pub struct Seen {
        pub requests: Vec<MachineRequest>,
        /// Machines whose deletion succeeded.
        pub deleted: Vec<String>,
        /// Deletions it failed on purpose.
        pub failed_deletes: u32,
        pub events: Vec<Event>,
    }

    type Hook = Arc<dyn Fn(MachineRequest, String) + Send + Sync>;

    struct Inner {
        behaviour: Mutex<Behaviour>,
        seen: Mutex<Seen>,
        hook: Option<Hook>,
        next: Mutex<u32>,
    }

    /// A running mock provisioner.
    #[derive(Clone)]
    pub struct Mock {
        pub url: String,
        inner: Arc<Inner>,
    }

    impl Mock {
        /// Serve on a free loopback port, on a thread of its own, until the
        /// process ends. `hook` gets each accepted request and the machine's
        /// id, `hook_after` after the answer.
        pub fn start(
            behaviour: Behaviour,
            hook: Option<Box<dyn Fn(MachineRequest, String) + Send + Sync>>,
        ) -> Self {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
            Self::serve(listener, behaviour, hook)
        }

        /// Serve on `listener`.
        pub fn serve(
            listener: std::net::TcpListener,
            behaviour: Behaviour,
            hook: Option<Box<dyn Fn(MachineRequest, String) + Send + Sync>>,
        ) -> Self {
            let url = format!("http://{}", listener.local_addr().expect("an address"));
            let inner = Arc::new(Inner {
                behaviour: Mutex::new(behaviour),
                seen: Mutex::new(Seen::default()),
                hook: hook.map(Hook::from),
                next: Mutex::new(0),
            });
            let app = Router::new()
                .route("/machines", post(create))
                .route("/machines/{id}", delete(remove))
                .route("/events", post(event))
                .with_state(inner.clone());
            listener.set_nonblocking(true).expect("non-blocking");
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("a runtime");
                rt.block_on(async move {
                    let l = tokio::net::TcpListener::from_std(listener).expect("a listener");
                    let _ = axum::serve(l, app).await;
                });
            });
            Self { url, inner }
        }

        pub fn seen(&self) -> Seen {
            self.inner.seen.lock().unwrap().clone()
        }

        /// Change what it answers from now on.
        pub fn set(&self, f: impl FnOnce(&mut Behaviour)) {
            f(&mut self.inner.behaviour.lock().unwrap());
        }
    }

    fn authorised(s: &Inner, h: &HeaderMap) -> bool {
        let Some(want) = s.behaviour.lock().unwrap().token.clone() else {
            return true;
        };
        h.get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("Bearer {want}"))
    }

    async fn create(
        State(s): State<Arc<Inner>>,
        h: HeaderMap,
        Json(r): Json<MachineRequest>,
    ) -> Result<Json<MachineAnswer>, StatusCode> {
        if !authorised(&s, &h) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let b = s.behaviour.lock().unwrap().clone();
        s.seen.lock().unwrap().requests.push(r.clone());
        if let Some(reason) = b.refuse {
            return Ok(Json(MachineAnswer {
                accepted: false,
                reason: Some(reason),
                ..MachineAnswer::default()
            }));
        }
        let id = {
            let mut n = s.next.lock().unwrap();
            *n += 1;
            format!("mock-{}", *n)
        };
        if let Some(hook) = s.hook.clone() {
            let (r, id) = (r.clone(), id.clone());
            std::thread::spawn(move || {
                std::thread::sleep(b.hook_after);
                hook(r, id);
            });
        }
        Ok(Json(MachineAnswer {
            accepted: true,
            machine: Some(id),
            address: Some(b.address),
            size: Some(b.size),
            estimate: Some(b.estimate),
            reason: None,
        }))
    }

    async fn remove(
        State(s): State<Arc<Inner>>,
        h: HeaderMap,
        UrlPath(id): UrlPath<String>,
    ) -> StatusCode {
        if !authorised(&s, &h) {
            return StatusCode::UNAUTHORIZED;
        }
        let mut seen = s.seen.lock().unwrap();
        if seen.failed_deletes < s.behaviour.lock().unwrap().fail_deletes {
            seen.failed_deletes += 1;
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        seen.deleted.push(id);
        StatusCode::NO_CONTENT
    }

    async fn event(State(s): State<Arc<Inner>>, h: HeaderMap, Json(e): Json<Event>) -> StatusCode {
        if !authorised(&s, &h) {
            return StatusCode::UNAUTHORIZED;
        }
        s.seen.lock().unwrap().events.push(e);
        StatusCode::NO_CONTENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(cpus: f64, memory: u64) -> Machine {
        Machine {
            request: "r".into(),
            machine: "m".into(),
            address: "10.0.0.1".into(),
            size: Some(Amount { cpus, memory }),
            repo: None,
            state: MachineState::Ready,
            requester: "a".into(),
            created: 0,
            deadline: 0,
            token_hash: String::new(),
            member: None,
            idle_since: None,
            alarm: None,
            error: None,
        }
    }

    #[test]
    fn caps_hold_on_count_cpu_and_memory() {
        let need = Need {
            cpus: 2.0,
            memory: 4 << 30,
            ..Need::default()
        };
        let s = Settings {
            max_nodes: 2,
            max_cpus: Some(16.0),
            max_memory: Some(40 << 30),
            ..Settings::default()
        };
        assert!(within_caps(&s, &[], &need).is_ok());
        let e =
            within_caps(&s, &[machine(8.0, 16 << 30), machine(8.0, 16 << 30)], &need).unwrap_err();
        assert!(e.contains("at most 2"), "{e}");
        let e = within_caps(&s, &[machine(15.0, 1 << 30)], &need).unwrap_err();
        assert!(e.contains("CPU"), "{e}");
        let e = within_caps(&s, &[machine(1.0, 38 << 30)], &need).unwrap_err();
        assert!(e.contains("GiB"), "{e}");
    }

    #[test]
    fn a_wait_is_the_estimate_within_bounds() {
        assert_eq!(wait_for(None), MAX_WAIT);
        assert_eq!(wait_for(Some(5)), MIN_WAIT);
        assert_eq!(wait_for(Some(90)), 90);
        assert_eq!(wait_for(Some(3600)), MAX_WAIT);
    }

    #[test]
    fn machines_keep_their_token_hash_on_disk_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = machine(4.0, 8 << 30);
        m.token_hash = token_hash("secret");
        save_machines(dir.path(), std::slice::from_ref(&m)).unwrap();
        assert_eq!(load_machines(dir.path()), vec![m.clone()]);
        let sent = serde_json::to_string(&m).unwrap();
        assert!(!sent.contains(&m.token_hash), "{sent}");
        assert_eq!(Settings::load(dir.path()), Settings::default());
    }
}
