import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'lanes.rs',[
("""pub struct Lanes {
    root_state: PathBuf,""",
"""/// While it lives, the main ports are moving to a lane: the main tree's
/// starts keep off them ([`Supervisor::set_handing_over`]).
struct Handing<'a>(&'a Supervisor);

impl Drop for Handing<'_> {
    fn drop(&mut self) {
        self.0.set_handing_over(false);
    }
}

pub struct Lanes {
    root_state: PathBuf,"""),
])
apply(W+'supervisor.rs',[
("""    pub(crate) fn service_running(&self, name: &str) -> bool {""",
"""    /// Building, starting or running: what a hand-over has to give back.
    pub(crate) fn service_active(&self, name: &str) -> bool {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|s| s.view.status.is_active())
    }

    pub(crate) fn service_running(&self, name: &str) -> bool {"""),
])
apply(W+'api.rs',[
("""async fn down(State(s): State<AppState>, Query(q): Query<DownQuery>) -> Response {
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.down(q.infra)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}""",
"""/// `204` once every service (and with `infra` every Compose group) stopped;
/// `409` naming what did not.
async fn down(State(s): State<AppState>, Query(q): Query<DownQuery>) -> Response {
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.down(q.infra)).await {
        Ok(failed) if failed.is_empty() => StatusCode::NO_CONTENT.into_response(),
        Ok(failed) => err(
            StatusCode::CONFLICT,
            format!("did not stop:\\n{}", failed.join("\\n")),
        ),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}"""),
])
apply(W+'logfiles.rs',[("    let mut push = |seg","    let push = |seg")])
print("ok")
