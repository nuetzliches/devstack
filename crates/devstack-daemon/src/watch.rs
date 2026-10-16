//! The file watcher (plan §3.2): changes in the working tree, matched against
//! what each service declares.
//!
//! - built services with `watch`: a candidate (ADR-0003, `Supervisor::changed`);
//! - dev servers with `restart_on`: a restart in place;
//! - Compose services with `restart_on`: a container restart;
//! - the stack file itself: the status says it changed since the daemon started
//!   (the daemon does not reload it);
//! - in a lane, anything: the copy its dev servers and tools run from follows
//!   (ADR-0024).
//!
//! Generated directories never count (`bin`, `obj`, `node_modules`, `target`,
//! `.git`, `.devstack`): a build in the repository would otherwise see its own
//! output as a change.

use crate::supervisor::Supervisor;
use devstack_config::model::Kind;
use globset::GlobSet;
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::sync::Arc;

/// Keeps the OS watch alive; dropping it stops watching.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
}

fn set(patterns: &[String]) -> Option<GlobSet> {
    devstack_build::watch_set(patterns).ok().flatten()
}

pub fn ignored(rel: &str) -> bool {
    rel.split('/').any(|c| {
        matches!(
            c,
            ".git" | ".devstack" | "node_modules" | "bin" | "obj" | "target"
        )
    })
}

/// Start watching the repository (only its top directory, for the stack file,
/// when nothing in the stack file asks for more); `None` when the OS watch
/// could not be set up.
pub fn start(sup: Arc<Supervisor>) -> Option<Watcher> {
    let platform = devstack_config::model::Platform::current();
    let in_lane = !sup.lane.is_empty();
    let runs = |s: &devstack_config::model::Service| {
        (s.platforms.is_empty() || s.platforms.contains(&platform)) && (!in_lane || s.lanes)
    };
    let built: Vec<(String, GlobSet)> = sup
        .stack
        .services
        .iter()
        .filter(|(_, s)| s.build.is_some() && runs(s))
        .filter_map(|(n, s)| set(&s.watch).map(|g| (n.clone(), g)))
        .collect();
    let devservers: Vec<(String, GlobSet)> = sup
        .stack
        .services
        .iter()
        .filter(|(_, s)| s.kind == Kind::Devserver && runs(s))
        .filter_map(|(n, s)| set(&s.restart_on).map(|g| (n.clone(), g)))
        .collect();
    // Containers belong to the main lane: a lane never restarts them (ADR-0024).
    let containers: Vec<(String, String, GlobSet)> = sup
        .stack
        .compose
        .iter()
        .filter(|_| sup.lane.is_empty())
        .flat_map(|(g, c)| {
            c.restart_on
                .iter()
                .filter_map(move |(s, p)| set(p).map(|set| (g.clone(), s.clone(), set)))
        })
        .collect();
    let lane = !sup.lane.is_empty();
    let only_stack_file =
        built.is_empty() && devservers.is_empty() && containers.is_empty() && !lane;
    let stack_rel = sup
        .stack_file()
        .strip_prefix(&sup.repo)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    let repo = sup.repo.clone();
    let me = sup.clone();
    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        // A new directory: inotify watches it only from now on, so files created
        // in it right away are missed. Report what it already contains.
        let mut paths = ev.paths;
        if matches!(ev.kind, EventKind::Create(_)) {
            let dirs: Vec<_> = paths.iter().filter(|p| p.is_dir()).cloned().collect();
            for d in dirs {
                let mut stack = vec![d];
                while let Some(dir) = stack.pop() {
                    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else {
                            paths.push(p);
                        }
                    }
                }
            }
        }
        for path in paths {
            let Ok(rel) = path.strip_prefix(&repo) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel.is_empty() || ignored(&rel) {
                continue;
            }
            if lane {
                me.copy_changed();
            }
            if stack_rel.as_deref() == Some(rel.as_str()) {
                me.stack_file_touched();
                continue;
            }
            for (n, g) in &built {
                if g.is_match(&rel) {
                    me.changed(n, &rel);
                }
            }
            for (n, g) in &devservers {
                if g.is_match(&rel) {
                    let n = n.clone();
                    me.debounce(format!("devserver:{n}"), move |s| {
                        if s.service_running(&n) {
                            let _ = s.restart(&n);
                        }
                    });
                }
            }
            for (group, service, g) in &containers {
                if g.is_match(&rel) {
                    let (group, service) = (group.clone(), service.clone());
                    me.debounce(format!("compose:{group}.{service}"), move |s| {
                        s.restart_container(&group, &service);
                    });
                }
            }
        }
    })
    .ok()?;
    let mode = if only_stack_file {
        RecursiveMode::NonRecursive
    } else {
        RecursiveMode::Recursive
    };
    inner.watch(&sup.repo, mode).ok()?;
    Some(Watcher { _inner: inner })
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_directories_do_not_count() {
        for p in [
            "src/Api/bin/Debug/x.dll",
            "neo/src/app/node_modules/a/b.js",
            ".devstack/lanes/main/src/x.cs",
            "obj/project.assets.json",
            ".git/index",
        ] {
            assert!(super::ignored(p), "{p}");
        }
        for p in [
            "src/Api/Program.cs",
            "src/binary-tools/x.cs",
            "docs/objects.md",
        ] {
            assert!(!super::ignored(p), "{p}");
        }
    }
}
