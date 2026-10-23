//! The candidate pipeline (ADR-0003) and the promotion gate (ADR-0013).

use super::{DEBOUNCE, Held, RESCAN, STOPPED, SUPERSEDED, Supervisor, first_lines};
use crate::state::{Candidate, Status, now_ms};
use devstack_config::model::Service;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often a candidate whose quiet period is over looks whether a restart
/// of its service is still under way.
const RESTART_POLL: Duration = Duration::from_millis(100);

impl Supervisor {
    /// The watcher saw `path` change, and `name` watches it. Debounced: a
    /// pipeline thread builds a candidate once the tree has been quiet for a
    /// moment; a newer change kills a build in progress and starts over.
    pub fn changed(self: &Arc<Self>, name: &str, path: &str) {
        if self.is_hot(name) {
            return; // its own watcher reloads it
        }
        if self.served_by().is_some() && self.stack.services.get(name).is_some_and(|s| s.proxied())
        {
            return; // another lane serves its port; no candidate may take it back
        }
        let spawn = {
            let mut changes = self.changes.lock().unwrap();
            let c = changes.entry(name.to_string()).or_default();
            c.paths.insert(path.to_string());
            c.last = Some(Instant::now());
            c.epoch += 1;
            !std::mem::replace(&mut c.worker, true)
        };
        let (serving, state) = {
            let slots = self.slots.lock().unwrap();
            let v = &slots[name].view;
            (
                slots[name].main.is_some(),
                v.candidate.as_ref().map(|c| c.state.clone()),
            )
        };
        // A starting or switching candidate finishes first; the change follows.
        // One that says `changed` already keeps its first path: every file of
        // a checkout or an install would be an event of its own (1925 in a
        // minute, seen in the pool's telemetry).
        if serving && !matches!(state.as_deref(), Some("starting" | "switching" | "changed")) {
            self.set_candidate(
                name,
                Some(Candidate::new("changed", None, Some(path.into()))),
            );
            if let Some(p) = self.proxy(name) {
                p.set_pending(Some("change detected".into()));
            }
        }
        if spawn {
            let (me, n) = (Arc::clone(self), name.to_string());
            let _ = std::thread::Builder::new()
                .name(format!("pipeline-{n}"))
                .spawn(move || me.pipeline(&n));
        }
    }

    fn pipeline(self: &Arc<Self>, name: &str) {
        loop {
            let (paths, epoch) = loop {
                let left = {
                    let mut changes = self.changes.lock().unwrap();
                    let c = changes.get_mut(name).expect("changed() created it");
                    let left = super::quiet_left(c.last, DEBOUNCE);
                    if left.is_zero() {
                        if c.absorbed >= c.epoch {
                            // A manual restart already took these changes in.
                            c.paths.clear();
                            c.worker = false;
                            c.last = None;
                            drop(changes);
                            self.clear_changed(name);
                            return;
                        }
                        // A restart under way may cover the change: one asked
                        // for right after an edit builds the tree with it, but
                        // the report can come after the restart began (FSEvents
                        // takes a few milliseconds, #233). Built now, the
                        // candidate is no build of main yet and takes over
                        // again, its jobs with it; built once the restart is
                        // done, it is the build main has and is dropped.
                        if !self.restart_under_way(name) {
                            break (std::mem::take(&mut c.paths), c.epoch);
                        }
                        RESTART_POLL
                    } else {
                        left
                    }
                };
                std::thread::sleep(left);
            };
            let superseded = self.candidate_from(name, &paths, epoch);
            let mut changes = self.changes.lock().unwrap();
            let c = changes.get_mut(name).expect("changed() created it");
            if superseded {
                c.paths.extend(paths); // the next candidate covers them too
            }
            if c.epoch == epoch {
                c.worker = false;
                c.last = None;
                drop(changes);
                // Kept for a start that may have finished meanwhile, before
                // the paths were back: it found nothing to take up.
                if superseded && self.service_running(name) {
                    self.resume_changes(name);
                }
                return;
            }
        }
    }

    /// Build a candidate from the current tree and promote it unless a gate
    /// holds it. Returns true if a newer change superseded the build, or if
    /// the paths wait for a start in progress or after an exit.
    fn candidate_from(self: &Arc<Self>, name: &str, paths: &BTreeSet<String>, epoch: u64) -> bool {
        let superseded = self.candidate_inner(name, paths, epoch);
        if let Some(s) = self.slots.lock().unwrap().get_mut(name) {
            s.in_flight = None;
        }
        superseded
    }

    fn candidate_inner(self: &Arc<Self>, name: &str, paths: &BTreeSet<String>, epoch: u64) -> bool {
        let Ok(svc) = self.service(name).cloned() else {
            return false;
        };
        let clear = || {
            self.set_candidate(name, None);
            if let Some(p) = self.proxy(name) {
                p.set_pending(None);
            }
        };
        // A newer change supersedes the build; a stop ends it.
        let stops = self.stop_epoch(name);
        let began = now_ms();
        // Nothing runs: nothing to replace. A start the user asks for builds
        // the tree anyway; but an instance that exited by itself starts
        // again from its old build, and a start in progress may have read
        // the tree before this change: the paths are kept, and that start
        // takes them up as a candidate once it runs (`resume_changes`).
        let (serving, coming) = {
            let slots = self.slots.lock().unwrap();
            let s = &slots[name];
            let coming = matches!(
                s.view.status,
                Status::Exited { .. } | Status::Building { .. } | Status::Starting
            );
            (s.main.is_some(), coming)
        };
        if !serving {
            clear();
            return coming;
        }
        let newer = || {
            self.stopped_since(name, stops)
                || self
                    .changes
                    .lock()
                    .unwrap()
                    .get(name)
                    .is_some_and(|c| c.epoch != epoch)
        };
        if let Some(p) = self.proxy(name) {
            p.set_pending(Some("building".into()));
        }
        let artifact = {
            let _op = self.op_locks[name].lock().unwrap();
            match self.build_locked(name, &svc, &newer, stops) {
                Ok(a) => a,
                Err(_) if self.stopped_since(name, stops) => {
                    clear();
                    return false;
                }
                Err(e) if e == SUPERSEDED => {
                    self.set_candidate(name, Some(Candidate::new("superseded", None, None)));
                    return true;
                }
                Err(e) => {
                    self.set_candidate(
                        name,
                        Some(Candidate::new("failed", None, Some(first_lines(&e, 8)))),
                    );
                    if let Some(p) = self.proxy(name) {
                        p.set_pending(Some("failed".into()));
                    }
                    // A held candidate's gate stays; there is nothing to
                    // promote until the tree builds again.
                    let reason = self
                        .held
                        .lock()
                        .unwrap()
                        .get(name)
                        .map(|h| h.reason.clone());
                    if let Some(reason) = reason {
                        self.hold(name, None, reason);
                    }
                    return false;
                }
            }
        };
        // A manual restart came in meanwhile and covers this change.
        if self
            .changes
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|c| c.absorbed >= epoch)
        {
            clear();
            return false;
        }
        // The change did not touch the service's inputs after all.
        if self.slots.lock().unwrap()[name].view.build_id.as_deref() == Some(artifact.0.as_str()) {
            crate::diag::info(
                "candidate_unchanged",
                serde_json::json!({
                    "lane": self.lane,
                    "service": name,
                    "build_id": artifact.0,
                    "paths": paths,
                    "why": "the changed files are not inputs of its build id (watch paths, build files above them)",
                }),
            );
            // The tree is back where main is: nothing waits for promote.
            self.release_held(name, None);
            clear();
            return false;
        }
        // A held candidate blocks later auto-promotions (ADR-0013): the
        // newer build carries its change too, and waits in its place.
        let waiting = self
            .held
            .lock()
            .unwrap()
            .get(name)
            .map(|h| h.reason.clone());
        if let Some(reason) = hold_reason(&svc, paths).or(waiting) {
            self.hold(name, Some(artifact.clone()), reason.clone());
            self.set_candidate(
                name,
                Some(Candidate::new(
                    "held",
                    Some(artifact.0.clone()),
                    Some(format!("{reason}; `devstack promote {name}` takes it over")),
                )),
            );
            if let Some(p) = self.proxy(name) {
                p.set_pending(Some(format!("held {}", artifact.0)));
            }
            return false;
        }
        if let Err(e) = self.take_over(name, &svc, artifact, stops, began) {
            // A stop came in meanwhile: the service stays stopped, and the
            // candidate goes with it.
            if self.stopped_since(name, stops) {
                clear();
                return false;
            }
            // replace() has set the candidate to failed already where it applies.
            let already = self.slots.lock().unwrap()[name]
                .view
                .candidate
                .as_ref()
                .is_some_and(|c| c.state == "failed");
            if !already {
                self.set_candidate(
                    name,
                    Some(Candidate::new("failed", None, Some(first_lines(&e, 8)))),
                );
            }
        }
        false
    }

    /// Run the jobs `svc` comes after (a schema migrator) unless they ran
    /// since `since` (Unix ms), then put the artifact into the main slot:
    /// through a candidate behind the proxy, or by a restart in place. A
    /// stop since `stops` ends it before that: what the user stopped stays
    /// stopped (ADR-0003).
    fn take_over(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        artifact: (String, PathBuf),
        stops: u64,
        since: u128,
    ) -> Result<(), String> {
        self.rerun_after_jobs(svc, since)?;
        if self.stopped_since(name, stops) {
            return Err(STOPPED.into());
        }
        if svc.proxied() && self.slots.lock().unwrap()[name].main.is_some() {
            return self.replace(name, svc, Some(artifact));
        }
        self.set_candidate(name, None);
        self.stop(name)?;
        self.start_with(name, Some(artifact))
    }

    /// A manual restart takes in every change seen so far: the watcher does
    /// not build them again.
    pub(super) fn absorb(&self, name: &str) {
        if let Some(c) = self.changes.lock().unwrap().get_mut(name) {
            c.paths.clear();
            c.absorbed = c.epoch;
        }
    }

    /// `name` runs (again): changes kept while it did not (an exit by
    /// itself, a start in progress) become a candidate now, as if the
    /// watcher had just seen them.
    pub(super) fn resume_changes(self: &Arc<Self>, name: &str) {
        let path = self.changes.lock().unwrap().get(name).and_then(|c| {
            (c.absorbed < c.epoch)
                .then(|| c.paths.first().cloned())
                .flatten()
        });
        if let Some(p) = path {
            self.changed(name, &p);
        }
    }

    /// Drop a "changed" marker that no candidate will follow.
    fn clear_changed(&self, name: &str) {
        let changed = self.slots.lock().unwrap()[name]
            .view
            .candidate
            .as_ref()
            .is_some_and(|c| c.state == "changed");
        if changed {
            self.set_candidate(name, None);
            if let Some(p) = self.proxy(name) {
                p.set_pending(None);
            }
        }
    }

    /// Take over a candidate that a promotion gate holds (ADR-0013).
    pub fn promote(self: &Arc<Self>, name: &str) -> Result<(), String> {
        if self.is_hot(name) {
            return Err(format!(
                "`{name}` runs in hot-reload mode: switch it off first"
            ));
        }
        let svc = self.service(name)?.clone();
        let Some(held) = self.held.lock().unwrap().get(name).cloned() else {
            return Err(format!("no candidate of `{name}` waits for promotion"));
        };
        let Some(artifact) = held.artifact else {
            return Err(format!(
                "the newest build of `{name}` failed: fix it, and the next build waits here for `devstack promote {name}`"
            ));
        };
        let id = artifact.0.clone();
        // Released only once it is main: a failed take-over keeps it held.
        self.take_over(name, &svc, artifact, self.stop_epoch(name), now_ms())?;
        self.release_held(name, Some(&id));
        Ok(())
    }

    /// The promotion gate of `name` opens (ADR-0013): its held candidate
    /// went into main, or a build of the tree as it is now did. `only`:
    /// just when the held build is that one.
    pub(super) fn release_held(&self, name: &str, only: Option<&str>) {
        let mut held = self.held.lock().unwrap();
        let matches = held.get(name).is_some_and(|h| {
            only.is_none_or(|id| h.artifact.as_ref().is_some_and(|(a, _)| a == id))
        });
        if matches {
            held.remove(name);
            self.save_held(&held);
        }
    }

    /// Hold `artifact` of `name` for `promote` (`None`: its newest build
    /// failed, the gate stays).
    fn hold(&self, name: &str, artifact: Option<(String, PathBuf)>, reason: String) {
        let mut held = self.held.lock().unwrap();
        held.insert(name.to_string(), Held { artifact, reason });
        self.save_held(&held);
    }

    fn save_held(&self, held: &BTreeMap<String, Held>) {
        let mut pers = self.persisted.lock().unwrap();
        pers.held = held
            .iter()
            .map(|(n, h)| {
                (
                    n.clone(),
                    crate::state::HeldRecord {
                        build_id: h
                            .artifact
                            .as_ref()
                            .map(|(id, _)| id.clone())
                            .unwrap_or_default(),
                        reason: h.reason.clone(),
                    },
                )
            })
            .collect();
        pers.save(&self.state_dir);
    }

    /// After a restore: what changed in the tree while no daemon watched it
    /// becomes a candidate, as if the watcher had seen it (a promotion gate
    /// sees those paths too).
    pub(super) fn follow_restore(self: &Arc<Self>, name: &str, svc: &Service) {
        let Some(before) = self.opened_manifest.lock().unwrap().clone() else {
            return;
        };
        let Some(watch) = devstack_build::watch_set(&svc.watch).ok().flatten() else {
            return; // without `watch`, edits make no candidates
        };
        let (me, name) = (Arc::clone(self), name.to_string());
        let _ = std::thread::Builder::new()
            .name(format!("restored-{name}"))
            .spawn(move || {
                let changed: Vec<String> = {
                    let mut snap = me.snapshot.lock().unwrap();
                    if snap.sync().is_err() {
                        return;
                    }
                    let now = &snap.manifest.files;
                    let mut paths: BTreeSet<&String> = now
                        .iter()
                        .filter(|(p, e)| before.files.get(*p) != Some(*e))
                        .map(|(p, _)| p)
                        .collect();
                    paths.extend(before.files.keys().filter(|p| !now.contains_key(*p)));
                    paths
                        .into_iter()
                        .filter(|p| {
                            !crate::watch::ignored(p)
                                && (watch.is_match(p.as_str()) || devstack_build::is_build_file(p))
                        })
                        .cloned()
                        .collect()
                };
                if !changed.is_empty() {
                    crate::diag::info(
                        "restored_tree_changed",
                        serde_json::json!({ "lane": me.lane, "service": name, "paths": changed }),
                    );
                }
                for p in changed {
                    me.changed(&name, &p);
                }
            });
    }
}

/// Why a built candidate waits for `promote` (ADR-0013): auto-promotion is
/// off, or the change touched paths the service declares as schema inputs.
fn hold_reason(svc: &Service, paths: &BTreeSet<String>) -> Option<String> {
    let p = svc.promote.as_ref()?;
    if !p.auto {
        return Some("auto-promotion is off".into());
    }
    let set = devstack_build::watch_set(&p.hold_if_changed)
        .ok()
        .flatten()?;
    if paths.contains(RESCAN) {
        return Some(
            "the file watcher lost events, so whether schema inputs changed is not known".into(),
        );
    }
    let hit: Vec<&str> = paths
        .iter()
        .filter(|x| set.is_match(x.as_str()))
        .map(String::as_str)
        .collect();
    (!hit.is_empty()).then(|| format!("changes schema inputs ({})", hit.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = "src/Site/index.html";

    /// A supervisor of `seed`, a job, and `a` and `b`, built services after
    /// it that watch [`PATH`]; none of them running.
    fn two_after_a_job() -> (tempfile::TempDir, tokio::runtime::Runtime, Arc<Supervisor>) {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("devstack.toml");
        let built = "build = \"git --version\"\noutput = \"src/Site\"\nwatch = [\"src/Site/**\"]\nrun = \"x\"\nafter = [\"seed\"]\n";
        let text = format!(
            "version = 1\n[stack]\nname = \"s\"\n[services.seed]\nkind = \"job\"\nrun = \"x\"\n[services.a]\n{built}[services.b]\n{built}"
        );
        let stack = devstack_config::from_str(&text, &file).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let sup = Supervisor::new(
            stack,
            file,
            d.path().to_path_buf(),
            d.path().join(".devstack"),
            devstack_proc::Options::default(),
            rt.handle().clone(),
        )
        .unwrap();
        (d, rt, sup)
    }

    /// Has the pipeline of `name` taken the changes it waited for?
    fn taken(sup: &Supervisor, name: &str) -> bool {
        sup.changes
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|c| c.paths.is_empty())
    }

    /// One edit, reported twice about 200 ms apart (FSEvents, #233), makes
    /// one deadline for every service it concerns: two services after the
    /// same job take their candidates at the same moment, the second report's
    /// time plus the quiet period, not one of them a poll later. The first
    /// report reaches the second service 20 ms after the first service (a
    /// busy daemon writing its log): the thread of the first then began well
    /// before the repeat, which made it miss its deadline by one poll.
    #[test]
    fn a_change_reported_twice_makes_one_deadline_for_every_service() {
        let (_d, _rt, sup) = two_after_a_job();

        sup.changed("a", PATH);
        std::thread::sleep(Duration::from_millis(20));
        sup.changed("b", PATH);
        std::thread::sleep(Duration::from_millis(200));
        let repeat = Instant::now();
        sup.changed("a", PATH);
        sup.changed("b", PATH);
        let deadline = repeat + DEBOUNCE;

        let mut fired: BTreeMap<&str, Instant> = BTreeMap::new();
        while fired.len() < 2 && repeat.elapsed() < DEBOUNCE * 3 {
            for name in ["a", "b"] {
                if !fired.contains_key(name) && taken(&sup, name) {
                    fired.insert(name, Instant::now());
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(fired.len(), 2, "both took their changes: {fired:?}");
        for (name, at) in &fired {
            assert!(*at >= deadline, "{name} fired before the quiet period");
            let late = *at - deadline;
            assert!(
                late < Duration::from_millis(100),
                "{name} fired {late:?} after its deadline"
            );
        }
        let (a, b) = (fired["a"], fired["b"]);
        let apart = a.max(b) - a.min(b);
        assert!(apart < Duration::from_millis(100), "{apart:?} apart");
        // Each pipeline took the change once, and is done with it.
        std::thread::sleep(Duration::from_millis(300));
        for name in ["a", "b"] {
            let changes = sup.changes.lock().unwrap();
            let c = &changes[name];
            assert_eq!(c.epoch, 2, "{name}");
            assert!(!c.worker && c.last.is_none(), "{name}");
        }
    }

    /// A change reported after a restart began waits for the restart, past
    /// its quiet period: the restart may have built it already (#233).
    #[test]
    fn a_candidate_waits_for_a_restart_under_way() {
        let (_d, _rt, sup) = two_after_a_job();
        *sup.restarts_under_way
            .lock()
            .unwrap()
            .entry("a".into())
            .or_default() += 1;
        sup.changed("a", PATH);
        std::thread::sleep(DEBOUNCE + Duration::from_millis(500));
        assert!(!taken(&sup, "a"), "it waits for the restart");
        *sup.restarts_under_way.lock().unwrap().get_mut("a").unwrap() -= 1;
        let done = Instant::now();
        while !taken(&sup, "a") && done.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(taken(&sup, "a"), "taken once the restart is done");
        assert!(
            done.elapsed() < Duration::from_secs(1),
            "{:?}",
            done.elapsed()
        );
    }
}
