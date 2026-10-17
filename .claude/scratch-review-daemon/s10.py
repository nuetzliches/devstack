import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'supervisor.rs',[
("""    watcher_error: Mutex<Option<String>>,""","""    watcher_error: Mutex<Option<(String, Instant)>>,"""),
("""            watcher_error: self.watcher_error.lock().unwrap().clone(),""",
"""            watcher_error: self
                .watcher_error
                .lock()
                .unwrap()
                .as_ref()
                .map(|(e, _)| e.clone()),"""),
("""    pub(crate) fn set_watcher_error(&self, error: Option<String>) {
        let mut cur = self.watcher_error.lock().unwrap();
        if *cur == error {
            return;
        }
        if let Some(e) = &error {
            crate::diag::warn(
                "watcher_failed",
                serde_json::json!({ "lane": self.lane, "error": e }),
            );
        }
        *cur = error;
    }""",
"""    pub(crate) fn set_watcher_error(&self, error: Option<String>) {
        let mut cur = self.watcher_error.lock().unwrap();
        if cur.as_ref().map(|(e, _)| e) == error.as_ref() {
            return;
        }
        if let Some(e) = &error {
            crate::diag::warn(
                "watcher_failed",
                serde_json::json!({ "lane": self.lane, "error": e }),
            );
        }
        *cur = error.map(|e| (e, Instant::now()));
    }

    /// The watcher delivers events again: a lost-events notice goes after a
    /// minute (long enough to be seen).
    pub(crate) fn watcher_ok(&self) {
        let mut cur = self.watcher_error.lock().unwrap();
        if cur
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(60))
        {
            *cur = None;
        }
    }"""),
])
apply(W+'watch.rs',[
("""        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        // A new directory""","""        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        me.watcher_ok();
        // A new directory"""),
])
print("ok")
