//! Artifacts: build, prune, pins, restore.

use super::{KEEP_ARTIFACTS, STOPPED, SUPERSEDED, Supervisor, tail};
use crate::state::{Candidate, Persisted, Status};
use devstack_build::{Outcome, build_id};
use devstack_config::model::Service;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl Supervisor {
    /// The store still has build `id` of `name`: retention (ADR-0002) did
    /// not remove it.
    pub fn has_artifact(&self, name: &str, id: &str) -> bool {
        self.builder.artifact(name, id).is_some()
    }

    /// Keep the newest artifacts of a service and everything something runs or
    /// may go back to (ADR-0002 retention): this tree's main, previous,
    /// candidate and held builds, and what every lane runs. Only the main tree
    /// prunes: the store is shared with the lanes.
    fn prune_artifacts(&self, name: &str) {
        if !self.layout.owns_artifacts() {
            return;
        }
        let mut pinned = self.pins(name);
        // What the started lanes run or may go back to.
        if let Some(lane_pins) = self.lane_pins.lock().unwrap().as_ref() {
            pinned.extend(lane_pins(name));
        }
        // What the lanes not started now restore at their next start (their
        // state.json).
        if let Ok(rd) = std::fs::read_dir(&self.state_dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("lane-") {
                    pinned.extend(Persisted::load(&e.path()).main.get(name).cloned());
                }
            }
        }
        let refs: Vec<&str> = pinned.iter().map(String::as_str).collect();
        let removed = self.builder.prune(name, KEEP_ARTIFACTS, &refs);
        if !removed.is_empty() {
            crate::diag::info(
                "artifacts_pruned",
                serde_json::json!({ "service": name, "removed": removed }),
            );
        }
    }

    /// The builds of a service this tree runs or may go back to: main,
    /// previous, candidate, held and the one restored at the next start.
    pub(crate) fn pins(&self, name: &str) -> BTreeSet<String> {
        let mut pinned: BTreeSet<String> = BTreeSet::new();
        if let Some(slot) = self.slots.lock().unwrap().get(name) {
            pinned.extend(slot.view.build_id.clone());
            pinned.extend(slot.previous.clone());
            pinned.extend(slot.in_flight.clone());
            pinned.extend(
                slot.view
                    .candidate
                    .as_ref()
                    .and_then(|c| c.build_id.clone()),
            );
        }
        if let Some((id, _)) = self
            .held
            .lock()
            .unwrap()
            .get(name)
            .and_then(|h| h.artifact.as_ref())
        {
            pinned.insert(id.clone());
        }
        pinned.extend(self.persisted.lock().unwrap().main.get(name).cloned());
        pinned
    }

    /// The artifact of the first start of `name` after a daemon start: the
    /// last one promoted to main, if it is still there (ADR-0016). What the
    /// tree holds now comes as a candidate, like any change.
    pub(super) fn restorable(&self, name: &str) -> Option<(String, PathBuf)> {
        if self.force_build.lock().unwrap().contains(name) {
            return None;
        }
        let id = self.persisted.lock().unwrap().main.get(name).cloned()?;
        self.builder.artifact(name, &id)?;
        Some((id.clone(), self.layout.artifact(name, &id)))
    }

    /// Sync the main lane and build `name` unless its artifact exists.
    /// `cancel` ends the build; it was a stop when one came in since
    /// `stops` (the error says [`STOPPED`] then, else [`SUPERSEDED`]).
    pub(super) fn build_locked(
        &self,
        name: &str,
        svc: &Service,
        cancel: &dyn Fn() -> bool,
        stops: u64,
    ) -> Result<(String, PathBuf), String> {
        // While an instance serves, the build is the candidate's business: the
        // service itself stays "healthy", it still answers (ADR-0003).
        let serving = self.slots.lock().unwrap()[name].main.is_some();
        let building = |id: Option<String>| {
            if serving {
                self.set_candidate(name, Some(Candidate::new("building", id, None)));
            } else {
                self.set_status(name, Status::Building { build_id: id });
            }
        };
        building(None);
        // `--rebuild` once, for every attempt below.
        let forced = self.force_build.lock().unwrap().remove(name);
        // All services of a tree share one snapshot: another one's build may
        // sync it while this one builds. An artifact whose inputs changed
        // under it is not what its id says (ADR-0002); it is not published,
        // and the build runs again on the tree as it is now.
        for _ in 0..3 {
            if let Some(a) = self.build_once(name, svc, cancel, stops, &building, forced)? {
                self.slots.lock().unwrap().get_mut(name).unwrap().in_flight = Some(a.0.clone());
                return Ok(a);
            }
        }
        Err("the sources changed under every build attempt (other builds synced the snapshot meanwhile); try again".into())
    }

    /// One build of [`Self::build_locked`]; `None` when the snapshot's
    /// inputs of `name` changed while it built (its artifact is discarded).
    fn build_once(
        &self,
        name: &str,
        svc: &Service,
        cancel: &dyn Fn() -> bool,
        stops: u64,
        building: &dyn Fn(Option<String>),
        forced: bool,
    ) -> Result<Option<(String, PathBuf)>, String> {
        let mut snap = self.snapshot.lock().unwrap();
        let stats = snap.sync().map_err(|e| e.to_string())?;
        if self.runs_from_copy() {
            self.log_sync(&format!("build of {name}"), &stats);
        }
        let base = build_id(&snap.manifest, svc).map_err(|e| e.to_string())?;
        let mut id = base.clone();
        // `--rebuild`: the same inputs once more, under `<id>-<n>`; the artifact
        // that exists may be running and is never overwritten (ADR-0002).
        if forced {
            let base = id.clone();
            id = (2..)
                .map(|n| format!("{base}-{n}"))
                .find(|i| self.builder.artifact(name, i).is_none())
                .expect("a free id");
            crate::diag::info(
                "build_forced",
                serde_json::json!({ "lane": self.lane, "service": name, "inputs": base, "build_id": id }),
            );
        } else if self.builder.artifact(name, &id).is_some() {
            crate::diag::info(
                "build_reused",
                serde_json::json!({
                    "lane": self.lane,
                    "service": name,
                    "build_id": id,
                    "why": "an artifact for these inputs exists (watch paths, build files above them, build command); `devstack restart --rebuild` builds anyway",
                }),
            );
        }
        if self.builder.artifact(name, &id).is_none() {
            building(Some(id.clone()));
            if let Some(job) = self.prepare_job(name, svc, &snap)? {
                drop(snap);
                self.run_prepare(name, job, cancel)?;
                snap = self.snapshot.lock().unwrap();
            }
            self.link_prepared(name, svc);
            let t0 = Instant::now();
            let mut running = self
                .builder
                .start_as("main", &snap, name, svc, id.clone())
                .map_err(|e| e.to_string())?;
            drop(snap); // other services may sync while this one builds
            // All services of a tree share one snapshot: another one's build
            // may sync it while this one builds. Published only while its
            // inputs are still the ones its id says, checked before the id
            // exists: no other supervisor can pick up a wrong artifact.
            let fresh = || {
                build_id(&self.snapshot.lock().unwrap().manifest, svc).is_ok_and(|now| now == base)
            };
            // A newer change supersedes the build: kill it (ADR-0003, spike b).
            let outcome = loop {
                if let Some(o) = self
                    .builder
                    .poll_checked(&mut running, &fresh)
                    .map_err(|e| e.to_string())?
                {
                    break o;
                }
                if cancel() {
                    self.builder.cancel(&mut running);
                    // A stop is no newer change (and no failed start: the
                    // diagnostic log keeps the two apart, ADR-0037).
                    if self.stopped_since(name, stops) {
                        self.build_done(name, &id, t0, "stopped");
                        return Err(STOPPED.into());
                    }
                    self.build_done(name, &id, t0, "superseded");
                    return Err(SUPERSEDED.into());
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let result = match &outcome {
                Outcome::Published { .. } => "ok",
                Outcome::Failed { .. } => "failed",
                Outcome::Cancelled { .. } => "cancelled",
                Outcome::Stale { .. } => "discarded",
            };
            self.build_done(name, &id, t0, result);
            match outcome {
                Outcome::Published { .. } => self.prune_artifacts(name),
                Outcome::Stale { .. } => {
                    crate::diag::warn(
                        "build_discarded",
                        serde_json::json!({
                            "lane": self.lane,
                            "service": name,
                            "build_id": id,
                            "why": "its inputs changed in the snapshot while it built (another build synced it); built again",
                        }),
                    );
                    return Ok(None);
                }
                Outcome::Failed { exit_code, log, .. } => {
                    // Point compiler messages at the repository, not at the
                    // snapshot, so they stay clickable in the IDE.
                    let snap_dir = self.layout.lane("main").join("src");
                    let text = build_errors(&log).replace(
                        &snap_dir.to_string_lossy().into_owned(),
                        &self.repo.to_string_lossy(),
                    );
                    return Err(format!("build failed with exit code {exit_code}\n{text}"));
                }
                Outcome::Cancelled { .. } => return Err("build cancelled".into()),
            }
        }
        Ok(Some((id.clone(), self.layout.artifact(name, &id))))
    }

    /// A build ended: how long it ran and how (ADR-0037 point 4).
    fn build_done(&self, name: &str, id: &str, t0: Instant, result: &str) {
        crate::diag::info(
            "build",
            serde_json::json!({
                "lane": self.lane,
                "service": name,
                "build_id": id,
                "result": result,
                "ms": t0.elapsed().as_millis() as u64,
            }),
        );
    }
}

/// What a failed build said: its error lines (compiler conventions `: error`,
/// `error:`, `error[`), each once and at most 20; without any, the last 15
/// lines. A .NET build ends with pages of warnings, which would bury the error.
fn build_errors(path: &Path) -> String {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut seen = BTreeSet::new();
    let errors: Vec<&str> = text
        .lines()
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains(": error") || lower.starts_with("error:") || lower.contains("error[")
        })
        .filter(|l| seen.insert(l.trim()))
        .take(20)
        .collect();
    if errors.is_empty() {
        tail(path, 15)
    } else {
        errors.join("\n")
    }
}

#[cfg(test)]
mod build_error_tests {
    #[test]
    fn errors_are_found_under_pages_of_warnings() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("build.log");
        let mut text = String::from("Program.cs(413,90): error CS1002: ; expected [Api.csproj]\n");
        for i in 0..40 {
            text.push_str(&format!(
                "x.csproj : warning NU1903: package {i} is vulnerable\n"
            ));
        }
        text.push_str("Program.cs(413,90): error CS1002: ; expected [Api.csproj]\n    0 Warning(s)\n    1 Error(s)\n");
        std::fs::write(&log, text).unwrap();
        assert_eq!(
            super::build_errors(&log),
            "Program.cs(413,90): error CS1002: ; expected [Api.csproj]"
        );
    }

    #[test]
    fn without_error_lines_the_end_of_the_log() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("build.log");
        std::fs::write(&log, "a\nb\nsomething went wrong\n").unwrap();
        assert!(super::build_errors(&log).ends_with("something went wrong"));
    }
}
