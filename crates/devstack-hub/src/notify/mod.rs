//! Desktop notifications from the hub (ADR-0045): it follows the event
//! stream of every running stack and of each started lane, as the dashboard
//! does, and tells the desktop what needs attention (`rules`): on Windows as
//! a toast, on Linux over the session bus (`devstack_desktop::notify`).
//!
//! What notifies is the person's choice, per machine (`notify.json` beside
//! the hub's state): each kind on or off, and a mute per stack or lane. The
//! same kind for the same service of the same stack or lane notifies once
//! in [`FOLD`].

mod rules;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_daemon::diag;
use devstack_desktop::notify::{Failure, Hooks, Note, Off, Sink};
use http_body_util::BodyExt;
use rules::{Kind, Lanes, Notice, Scope};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_stream::StreamExt;

use crate::{AppState, err};

/// The same kind for the same service of the same stack or lane notifies
/// once in this time: a crash loop, or a build failing on every save, is
/// one notification.
const FOLD: Duration = Duration::from_secs(300);

/// How long to wait before following a stream again that ended or could
/// not be opened (a daemon restarting, a lane on a node unreachable): from
/// [`RETRY_MIN`], doubled up to [`RETRY_MAX`].
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

const FILE: &str = "notify.json";

/// What a notification opens: the dashboard on a stack (its main tree, or a
/// lane) and a service, or its home page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Target {
    pub stack: Option<String>,
    pub lane: Option<String>,
    pub service: Option<String>,
    /// The pool area instead (guest lanes).
    pub pool: bool,
}

impl Target {
    /// The dashboard's route for it (hash history). `lane=` (empty) shows
    /// the main working tree, whichever lane was shown last.
    pub fn path(&self) -> String {
        if self.pool {
            return "/#/pool".into();
        }
        let Some(stack) = &self.stack else {
            return "/#/".into();
        };
        let mut p = format!("/#/s/{}", enc(stack));
        if let Some(s) = &self.service {
            p.push_str(&format!("/services/{}", enc(s)));
        }
        p.push_str(&format!(
            "?lane={}",
            enc(self.lane.as_deref().unwrap_or(""))
        ));
        p
    }

    /// The `devstack://open` link that opens it (`devstack open-url`).
    pub fn link(&self) -> String {
        if self.pool {
            return "devstack://open?page=pool".into();
        }
        let mut q = Vec::new();
        for (k, v) in [
            ("stack", &self.stack),
            ("lane", &self.lane),
            ("service", &self.service),
        ] {
            if let Some(v) = v {
                q.push(format!("{k}={}", enc(v)));
            }
        }
        if q.is_empty() {
            "devstack://open".into()
        } else {
            format!("devstack://open?{}", q.join("&"))
        }
    }
}

/// Percent-encoding of everything but unreserved characters.
fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A notification for `target`, as the desktop shows it.
fn note(title: String, context: String, body: String, target: &Target, tag: String) -> Note {
    Note {
        title,
        context,
        body,
        route: target.path(),
        link: target.link(),
        tag,
    }
}

/// `DEVSTACK_NO_NOTIFY` is set and not `0`: the hub shows nothing and
/// registers nothing for it (tests, a machine someone does not sit at).
fn switched_off() -> bool {
    std::env::var_os("DEVSTACK_NO_NOTIFY").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Shows the dashboard at a route for a click the hub hears itself; `Err`
/// with why it could not.
pub(crate) type Open = devstack_desktop::notify::Open;

/// The sink of this platform (`devstack_desktop::notify`), and on Windows
/// the registration its toasts need. `open` shows the dashboard for a click
/// the hub hears itself (Linux); a Windows toast opens it through a link.
pub(crate) fn platform_sink(port: u16, open: Open) -> Box<dyn Sink> {
    if switched_off() {
        return Box::new(Off("switched off (DEVSTACK_NO_NOTIFY)"));
    }
    devstack_desktop::notify::sink(
        port,
        Hooks {
            open,
            failed: std::sync::Arc::new(failed),
        },
    )
}

/// What went wrong in the sink: to the hub's log, and a notification that
/// could not be shown to the dashboard too.
fn failed(f: Failure) {
    match f {
        Failure::NotShown(e) => {
            diag::warn("notify_failed", serde_json::json!({ "error": e }));
            crate::notice::tell(
                "warn",
                format!("A desktop notification could not be shown: {e}"),
            );
        }
        Failure::NotRegistered(e) => {
            diag::warn("notify_register_failed", serde_json::json!({ "error": e }))
        }
    }
}

/// The person's choices (`notify.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct Settings {
    /// Kind -> on; a kind not named has its default.
    #[serde(default)]
    kinds: BTreeMap<String, bool>,
    /// Stack ids, and `<stack id>/<lane>`.
    #[serde(default)]
    muted: BTreeSet<String>,
}

impl Settings {
    fn on(&self, kind: Kind) -> bool {
        self.kinds
            .get(kind.key())
            .copied()
            .unwrap_or(kind.default_on())
    }

    fn muted(&self, stack: &str, lane: Option<&str>) -> bool {
        self.muted.contains(stack)
            || lane.is_some_and(|l| self.muted.contains(&format!("{stack}/{l}")))
    }
}

/// Settings, what was told lately, and where it goes.
pub(crate) struct Center {
    path: PathBuf,
    settings: Mutex<Settings>,
    /// Fold key -> when it last notified.
    recent: Mutex<HashMap<String, Instant>>,
    sink: Box<dyn Sink>,
}

impl Center {
    pub fn new(dir: &Path, sink: Box<dyn Sink>) -> Self {
        let path = dir.join(FILE);
        let settings = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            settings: Mutex::new(settings),
            recent: Mutex::new(HashMap::new()),
            sink,
        }
    }

    /// Tell `n`, from the stream of stack `stack` (named `name`) in `lane`
    /// (`None`: its main tree), unless its kind is off, the stack or lane
    /// is muted, or the same was told within [`FOLD`]. Whether it was.
    fn deliver(&self, stack: &str, name: &str, lane: Option<&str>, n: Notice) -> bool {
        // A lane event of the main tree's stream is about that lane.
        let lane = n.lane.as_deref().or(lane);
        {
            let s = self.settings.lock().unwrap();
            if !s.on(n.kind) || s.muted(stack, lane) {
                return false;
            }
        }
        let key = format!(
            "{stack}/{}/{}/{}",
            lane.unwrap_or(""),
            n.service.as_deref().unwrap_or(""),
            n.kind.key()
        );
        {
            let mut recent = self.recent.lock().unwrap();
            let now = Instant::now();
            recent.retain(|_, at| now.duration_since(*at) < FOLD);
            if recent.contains_key(&key) {
                return false;
            }
            recent.insert(key.clone(), now);
        }
        diag::info(
            "notice",
            serde_json::json!({
                "kind": n.kind.key(), "stack": name, "lane": lane, "service": n.service,
                "shown": self.sink.status().is_ok(),
            }),
        );
        let target = Target {
            stack: Some(stack.to_string()),
            lane: lane.map(str::to_string),
            service: n.service,
            pool: false,
        };
        self.sink.show(note(
            n.title,
            match lane {
                Some(l) => format!("{name} · lane {l}"),
                None => name.to_string(),
            },
            n.body,
            &target,
            blake3::hash(key.as_bytes()).to_hex()[..16].to_string(),
        ));
        true
    }

    /// A lane of another member started running on this machine
    /// (`guests.rs`). Once per lease; whether it was told.
    pub fn guest_started(&self, g: &crate::guests::Guest) -> bool {
        if !self.settings.lock().unwrap().on(Kind::GuestLane) {
            return false;
        }
        let key = format!("guest/{}/{}", g.pool, g.lease);
        diag::info(
            "notice",
            serde_json::json!({
                "kind": Kind::GuestLane.key(), "pool": g.pool, "owner": g.owner,
                "repo": g.repo, "lane": g.lane, "shown": self.sink.status().is_ok(),
            }),
        );
        let pool = if g.pool == devstack_node::member::DEFAULT_POOL {
            String::new()
        } else {
            format!(" · pool {}", g.pool)
        };
        self.sink.show(note(
            format!("{}'s lane {} runs on this machine", g.owner, g.lane),
            format!("{}{pool}", g.repo),
            "The pool area shows it; pausing the node there takes no new lanes.".into(),
            &Target {
                pool: true,
                ..Target::default()
            },
            blake3::hash(key.as_bytes()).to_hex()[..16].to_string(),
        ));
        true
    }

    /// What `GET /hub/notify` answers.
    fn view(&self) -> Value {
        let s = self.settings.lock().unwrap();
        let status = self.sink.status();
        serde_json::json!({
            "available": status.is_ok(),
            "unavailable": status.err(),
            "trouble": self.sink.trouble(),
            "kinds": Kind::ALL.iter().map(|k| serde_json::json!({
                "kind": k.key(),
                "label": k.label(),
                "on": s.on(*k),
                "default": k.default_on(),
            })).collect::<Vec<_>>(),
            "muted": s.muted,
        })
    }

    fn update(&self, u: Update) -> Result<(), String> {
        let mut s = self.settings.lock().unwrap();
        let mut next = s.clone();
        for (k, on) in u.kinds.unwrap_or_default() {
            let kind =
                Kind::from_key(&k).ok_or_else(|| format!("no kind of notification `{k}`"))?;
            // Kept only where it differs from the default, so a changed
            // default reaches whoever never chose.
            if on == kind.default_on() {
                next.kinds.remove(&k);
            } else {
                next.kinds.insert(k, on);
            }
        }
        if let Some(muted) = u.muted {
            if let Some(bad) = muted.iter().find(|m| !valid_scope(m)) {
                return Err(format!("`{bad}` is no stack id or `<stack id>/<lane>`"));
            }
            next.muted = muted;
        }
        if next != *s {
            let json = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, json)
                .and_then(|_| std::fs::rename(&tmp, &self.path))
                .map_err(|e| format!("{}: {e}", self.path.display()))?;
            *s = next;
        }
        Ok(())
    }
}

/// A stack id (hex), or one with a lane's name.
fn valid_scope(s: &str) -> bool {
    let (stack, lane) = s.split_once('/').map_or((s, None), |(a, b)| (a, Some(b)));
    !stack.is_empty()
        && stack.chars().all(|c| c.is_ascii_hexdigit())
        && lane.is_none_or(|l| {
            !l.is_empty()
                && l.len() <= 200
                && !l.contains(['/', '\\'])
                && !l.chars().any(char::is_control)
        })
}

#[derive(Deserialize)]
pub(crate) struct Update {
    kinds: Option<BTreeMap<String, bool>>,
    muted: Option<BTreeSet<String>>,
}

/// `GET /hub/notify`.
pub(crate) async fn get(State(s): State<AppState>) -> Response {
    Json(s.notify.view()).into_response()
}

/// `PUT /hub/notify` with `{"kinds"?: {kind: bool}, "muted"?: [scope]}`.
pub(crate) async fn put(State(s): State<AppState>, Json(u): Json<Update>) -> Response {
    match s.notify.update(u) {
        Ok(()) => Json(s.notify.view()).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// `POST /hub/notify/test`: one notification, whatever the settings say.
pub(crate) async fn test(State(s): State<AppState>) -> Response {
    if let Err(why) = s.notify.sink.status() {
        return err(StatusCode::SERVICE_UNAVAILABLE, why);
    }
    s.notify.sink.show(note(
        "devstack notifications work".into(),
        "A test from the dashboard hub".into(),
        "A click opens the dashboard.".into(),
        &Target::default(),
        "test".into(),
    ));
    StatusCode::NO_CONTENT.into_response()
}

// ------------------------------------------------------- following streams

/// Ends the task it holds when dropped: a stack's lane streams end with it.
struct Task(tokio::task::AbortHandle);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn spawn(f: impl std::future::Future<Output = ()> + Send + 'static) -> Task {
    Task(tokio::spawn(f).abort_handle())
}

/// Follow every running stack, for the hub's life: a stack that starts is
/// followed, one that stops (or whose daemon was replaced) no longer.
pub(crate) fn start(s: AppState) {
    tokio::spawn(async move {
        let mut stacks = s.stacks.clone();
        let mut followed: HashMap<String, (u32, Task)> = HashMap::new();
        loop {
            let running: Vec<(String, String, u32)> = stacks
                .borrow_and_update()
                .iter()
                .filter_map(|e| Some((e.id.clone(), e.name.clone(), e.daemon.as_ref()?.pid)))
                .collect();
            followed.retain(|id, (pid, _)| running.iter().any(|(i, _, p)| i == id && p == pid));
            for (id, name, pid) in running {
                if let Entry::Vacant(e) = followed.entry(id) {
                    let task = spawn(follow_stack(s.clone(), e.key().clone(), name));
                    e.insert((pid, task));
                }
            }
            if stacks.changed().await.is_err() {
                return;
            }
        }
    });
}

/// The main tree's stream of stack `id`, and through its `lanes` events
/// the streams of its started lanes.
async fn follow_stack(s: AppState, id: String, name: String) {
    let mut wait = RETRY_MIN;
    loop {
        let mut lanes = Lanes::default();
        let mut scope = Scope::default();
        let mut followed: HashMap<String, Task> = HashMap::new();
        if let Some(mut events) = open(&s, &id, "/v1/events").await {
            if let Some(v) = get_json(&s, &id, "/v1/lanes").await {
                lanes.baseline(&v);
                follow_lanes(&s, &id, &name, &v, &mut followed);
            }
            while let Some(text) = events.next().await {
                wait = RETRY_MIN;
                let Ok(ev) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let notices = match ev["type"].as_str() {
                    Some("usage") => continue,
                    Some("lanes") => {
                        follow_lanes(&s, &id, &name, &ev["lanes"], &mut followed);
                        lanes.apply(&ev["lanes"])
                    }
                    _ => scope.apply(&ev),
                };
                for n in notices {
                    s.notify.deliver(&id, &name, None, n);
                }
            }
        }
        drop(followed);
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RETRY_MAX);
    }
}

/// Follow the started lanes in `lanes`, no longer those that stopped.
fn follow_lanes(
    s: &AppState,
    id: &str,
    name: &str,
    lanes: &Value,
    followed: &mut HashMap<String, Task>,
) {
    let started = Lanes::started(lanes);
    followed.retain(|l, _| started.contains(l));
    for lane in started {
        if let Entry::Vacant(e) = followed.entry(lane) {
            let lane = e.key().clone();
            e.insert(spawn(follow_lane(
                s.clone(),
                id.to_string(),
                name.to_string(),
                lane,
            )));
        }
    }
}

/// A lane's own stream (on this machine or, passed through, on a node).
async fn follow_lane(s: AppState, id: String, name: String, lane: String) {
    let path = format!("/v1/lanes/{}/v1/events", enc(&lane));
    let mut wait = RETRY_MIN;
    loop {
        let mut scope = Scope::default();
        if let Some(mut events) = open(&s, &id, &path).await {
            while let Some(text) = events.next().await {
                wait = RETRY_MIN;
                let Ok(ev) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if ev["type"] == "usage" {
                    continue;
                }
                for n in scope.apply(&ev) {
                    s.notify.deliver(&id, &name, Some(&lane), n);
                }
            }
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RETRY_MAX);
    }
}

type Events = std::pin::Pin<Box<dyn tokio_stream::Stream<Item = String> + Send>>;

/// The event stream at `path` of stack `id`'s daemon, one `data` per item.
async fn open(s: &AppState, id: &str, path: &str) -> Option<Events> {
    let resp = crate::daemon_get(s, id, path).await.ok()?;
    if resp.status() != StatusCode::OK {
        return None;
    }
    Some(Box::pin(crate::ws::sse_data(
        resp.into_body().into_data_stream(),
    )))
}

async fn get_json(s: &AppState, id: &str, path: &str) -> Option<Value> {
    let resp = crate::daemon_get(s, id, path).await.ok()?;
    if resp.status() != StatusCode::OK {
        return None;
    }
    let body = resp.into_body().collect().await.ok()?.to_bytes();
    serde_json::from_slice(&body).ok()
}

/// Remove what the hub registered with the system for its notifications
/// (`devstack self uninstall`).
pub fn unregister() {
    devstack_desktop::notify::unregister();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Default)]
    struct Recorded(Arc<Mutex<Vec<Note>>>);

    impl Sink for Recorded {
        fn status(&self) -> Result<(), String> {
            Ok(())
        }
        fn show(&self, n: Note) {
            self.0.lock().unwrap().push(n);
        }
    }

    fn center(dir: &Path) -> (Center, Arc<Mutex<Vec<Note>>>) {
        let shown = Arc::new(Mutex::new(Vec::new()));
        let c = Center::new(dir, Box::new(Recorded(shown.clone())));
        (c, shown)
    }

    fn notice(kind: Kind, service: &str) -> Notice {
        Notice {
            kind,
            service: Some(service.into()),
            lane: None,
            title: format!("{service}: x"),
            body: String::new(),
        }
    }

    #[test]
    fn the_same_thing_notifies_once_in_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let (c, shown) = center(dir.path());
        assert!(c.deliver("ab12", "shop", None, notice(Kind::ServiceFailed, "api")));
        assert!(!c.deliver("ab12", "shop", None, notice(Kind::ServiceFailed, "api")));
        // Another service, kind, lane: each its own.
        assert!(c.deliver("ab12", "shop", None, notice(Kind::ServiceFailed, "web")));
        assert!(c.deliver("ab12", "shop", None, notice(Kind::BuildFailed, "api")));
        assert!(c.deliver(
            "ab12",
            "shop",
            Some("feat-x"),
            notice(Kind::ServiceFailed, "api")
        ));
        let shown = shown.lock().unwrap();
        assert_eq!(shown.len(), 4);
        assert_eq!(shown[3].context, "shop · lane feat-x");
        assert_eq!(shown[3].route, "/#/s/ab12/services/api?lane=feat-x");
        assert_eq!(
            shown[3].link,
            "devstack://open?stack=ab12&lane=feat-x&service=api"
        );
    }

    #[test]
    fn kinds_off_and_muted_scopes_stay_quiet_and_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (c, shown) = center(dir.path());
        // Off by default.
        assert!(!c.deliver("ab12", "shop", None, notice(Kind::Live, "api")));
        c.update(Update {
            kinds: Some([("live".into(), true), ("held".into(), false)].into()),
            muted: Some(["cd34".into(), "ab12/agent-1".into()].into()),
        })
        .unwrap();
        assert!(c.deliver("ab12", "shop", None, notice(Kind::Live, "api")));
        assert!(!c.deliver("ab12", "shop", None, notice(Kind::Held, "api")));
        assert!(!c.deliver("cd34", "other", None, notice(Kind::BuildFailed, "api")));
        assert!(!c.deliver(
            "ab12",
            "shop",
            Some("agent-1"),
            notice(Kind::BuildFailed, "api")
        ));
        assert!(c.deliver(
            "ab12",
            "shop",
            Some("agent-2"),
            notice(Kind::BuildFailed, "api")
        ));
        assert_eq!(shown.lock().unwrap().len(), 2);

        // Kept, and only what differs from the defaults.
        let (again, _) = center(dir.path());
        let v = again.view();
        let on = |k: &str| {
            v["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["kind"] == k)
                .unwrap()["on"]
                .clone()
        };
        assert_eq!(on("live"), true);
        assert_eq!(on("held"), false);
        assert_eq!(on("build_failed"), true);
        assert_eq!(v["muted"], serde_json::json!(["ab12/agent-1", "cd34"]));
        let file: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(
            file["kinds"],
            serde_json::json!({ "held": false, "live": true })
        );
    }

    #[test]
    fn the_view_says_why_nothing_shows_and_what_does_not_work() {
        struct Troubled;
        impl Sink for Troubled {
            fn status(&self) -> Result<(), String> {
                Ok(())
            }
            fn trouble(&self) -> Option<String> {
                Some("no name".into())
            }
            fn show(&self, _: Note) {}
        }
        let dir = tempfile::tempdir().unwrap();
        let v = Center::new(dir.path(), Box::new(Troubled)).view();
        assert_eq!(
            (&v["available"], &v["trouble"]),
            (&true.into(), &"no name".into())
        );
        let v = Center::new(dir.path(), Box::new(Off("switched off"))).view();
        assert_eq!(v["unavailable"], "switched off");
        assert_eq!(v["trouble"], Value::Null);
    }

    #[test]
    fn bad_updates_are_refused_and_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _) = center(dir.path());
        assert!(
            c.update(Update {
                kinds: Some([("nope".into(), true)].into()),
                muted: None
            })
            .is_err()
        );
        for bad in ["", "shop", "ab12/", "ab12/a/b", "../x"] {
            assert!(
                c.update(Update {
                    kinds: None,
                    muted: Some([bad.into()].into())
                })
                .is_err(),
                "{bad}"
            );
        }
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn targets_become_dashboard_paths_and_links() {
        let t = Target {
            stack: Some("ab12".into()),
            lane: Some("feat x".into()),
            service: Some("api".into()),
            pool: false,
        };
        assert_eq!(t.path(), "/#/s/ab12/services/api?lane=feat%20x");
        assert_eq!(
            t.link(),
            "devstack://open?stack=ab12&lane=feat%20x&service=api"
        );
        let main = Target {
            stack: Some("ab12".into()),
            ..Target::default()
        };
        assert_eq!(main.path(), "/#/s/ab12?lane=");
        assert_eq!(main.link(), "devstack://open?stack=ab12");
        assert_eq!(Target::default().path(), "/#/");
        assert_eq!(Target::default().link(), "devstack://open");
        let pool = Target {
            pool: true,
            ..Target::default()
        };
        assert_eq!(
            (pool.path().as_str(), pool.link().as_str()),
            ("/#/pool", "devstack://open?page=pool")
        );
    }
}
