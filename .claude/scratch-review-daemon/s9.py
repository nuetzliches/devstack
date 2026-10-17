import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'watch.rs',[
("""/// Start watching the repository (only its top directory, for the stack file,
/// when nothing in the stack file asks for more); `None` when the OS watch
/// could not be set up.
pub fn start(sup: Arc<Supervisor>) -> Option<Watcher> {""",
"""/// Start watching the repository (only its top directory, for the stack file,
/// when nothing in the stack file asks for more); `None` when the OS watch
/// could not be set up, which the status and the log then say. When the OS
/// drops events (an overflow, a checkout of thousands of files), every built
/// service gets a candidate, as if all of its files had changed.
pub fn start(sup: Arc<Supervisor>) -> Option<Watcher> {
    match watch(sup.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            sup.set_watcher_error(Some(format!(
                "the file watcher could not be set up on {}: {e}; edits make no candidates (restart a service to pick them up)",
                sup.repo.display()
            )));
            None
        }
    }
}

fn watch(sup: Arc<Supervisor>) -> Result<Watcher, String> {"""),
("""    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }""",
"""    let rescan = {
        let (me, built) = (sup.clone(), built.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>());
        move |why: String| {
            me.set_watcher_error(Some(format!(
                "the file watcher lost events ({why}); every built service was rebuilt as a candidate"
            )));
            if lane {
                me.copy_changed();
            }
            for n in &built {
                me.changed(n, crate::supervisor::RESCAN);
            }
        }
    };
    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let ev = match res {
            Ok(ev) if ev.need_rescan() => return rescan("the OS asked for a rescan".into()),
            Ok(ev) => ev,
            Err(e) => return rescan(e.to_string()),
        };
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }"""),
("""    let repo = sup.repo.clone();
    let me = sup.clone();""",
"""    let repo = sup.repo.clone();
    let me = sup.clone();
    let lane_copy = lane;"""),
("""            if lane {
                me.copy_changed();
            }
            if stack_rel.as_deref() == Some(rel.as_str()) {""",
"""            if lane_copy {
                me.copy_changed();
            }
            if stack_rel.as_deref() == Some(rel.as_str()) {"""),
("""    })
    .ok()?;
    let mode = if only_stack_file {
        RecursiveMode::NonRecursive
    } else {
        RecursiveMode::Recursive
    };
    inner.watch(&sup.repo, mode).ok()?;
    Some(Watcher { _inner: inner })
}""",
"""    })
    .map_err(|e| e.to_string())?;
    let mode = if only_stack_file {
        RecursiveMode::NonRecursive
    } else {
        RecursiveMode::Recursive
    };
    inner.watch(&sup.repo, mode).map_err(|e| e.to_string())?;
    sup.set_watcher_error(None);
    Ok(Watcher { _inner: inner })
}"""),
])
print("ok")
