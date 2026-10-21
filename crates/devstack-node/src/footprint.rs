//! What a repository's lanes took where they ran (ADR-0049 points 2 and 3).
//! A node measures each lease's run and reports it; the coordinator keeps
//! the last [`RUNS`] runs per repository and profile in `footprints.json`
//! of its directory, and a lane's need is the most each part took in them.
//! Only the running coordinator writes the file (a temporary file, then
//! rename).

use crate::share::Need;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The runs a footprint is the most of.
pub const RUNS: usize = 5;

/// Repositories and profiles kept; past this the least recently run goes.
const KEYS: usize = 1000;

/// A lease's run as its node measured it, in the node's report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// The lease.
    pub id: String,
    pub repo: String,
    /// The profile it runs with (ADR-0041); absent: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The most it took so far: CPU averaged over a minute, memory, the
    /// repository's images and its slot.
    pub peak: Need,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    /// Unix seconds, when it was last reported.
    at: u64,
    peak: Need,
}

pub struct Footprints {
    path: PathBuf,
    by: Mutex<BTreeMap<String, Vec<Entry>>>,
}

fn key(repo: &str, profile: Option<&str>) -> String {
    format!("{repo}\n{}", profile.unwrap_or(""))
}

impl Footprints {
    /// The footprints kept in `dir`; none when there is no file yet or it
    /// does not read.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("footprints.json");
        let by = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            by: Mutex::new(by),
        }
    }

    /// Take `run`: a run known already is raised to it, a new one comes
    /// after the others and the oldest beyond [`RUNS`] goes. Whether
    /// anything changed. GPUs are declared, not measured: a run says
    /// nothing about them.
    pub fn record(&self, run: &Run, now: u64) -> bool {
        let peak = Need {
            gpus: None,
            ..run.peak
        };
        let mut by = self.by.lock().unwrap();
        let runs = by
            .entry(key(&run.repo, run.profile.as_deref()))
            .or_default();
        let changed = match runs.iter_mut().find(|e| e.id == run.id) {
            Some(e) => {
                let raised = e.peak.max(peak);
                let changed = raised != e.peak;
                e.peak = raised;
                e.at = now;
                changed
            }
            None => {
                runs.push(Entry {
                    id: run.id.clone(),
                    at: now,
                    peak,
                });
                if runs.len() > RUNS {
                    runs.remove(0);
                }
                true
            }
        };
        if by.len() > KEYS
            && let Some(oldest) = by
                .iter()
                .min_by_key(|(_, v)| v.iter().map(|e| e.at).max().unwrap_or(0))
                .map(|(k, _)| k.clone())
        {
            by.remove(&oldest);
        }
        changed
    }

    /// The most each part took in the last runs of `repo` with `profile`,
    /// and how many runs that is; `None`: none ran in the pool yet.
    pub fn get(&self, repo: &str, profile: Option<&str>) -> Option<(Need, usize)> {
        let by = self.by.lock().unwrap();
        let runs = by.get(&key(repo, profile)).filter(|r| !r.is_empty())?;
        let most = runs.iter().fold(Need::default(), |a, e| a.max(e.peak));
        Some((most, runs.len()))
    }

    pub fn save(&self) -> std::io::Result<()> {
        let text =
            serde_json::to_vec_pretty(&*self.by.lock().unwrap()).expect("footprints serialise");
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn run(id: &str, memory: u64, disk_lane: u64) -> Run {
        Run {
            id: id.into(),
            repo: "shop".into(),
            profile: None,
            peak: Need {
                cpus: 1.0,
                memory,
                disk_lane,
                ..Need::default()
            },
        }
    }

    #[test]
    fn the_most_of_the_last_five_runs() {
        let dir = tempfile::tempdir().unwrap();
        let f = Footprints::load(dir.path());
        assert_eq!(f.get("shop", None), None);
        assert!(f.record(&run("a", 8 * GIB, GIB), 1));
        for (i, id) in ["b", "c", "d", "e"].iter().enumerate() {
            assert!(f.record(&run(id, 2 * GIB, 3 * GIB), 2 + i as u64));
        }
        let (n, runs) = f.get("shop", None).unwrap();
        assert_eq!((n.memory, n.disk_lane, runs), (8 * GIB, 3 * GIB, 5));
        // A sixth run: the first, with the outlier, goes.
        assert!(f.record(&run("f", 2 * GIB, GIB), 9));
        assert_eq!(f.get("shop", None).unwrap().0.memory, 2 * GIB);
        // A run reported again is raised, never lowered.
        assert!(!f.record(&run("f", GIB, GIB), 10));
        assert!(f.record(&run("f", 4 * GIB, GIB), 11));
        assert_eq!(f.get("shop", None).unwrap().0.memory, 4 * GIB);
        // Another profile is another footprint.
        assert_eq!(f.get("shop", Some("acceptance")), None);
        f.save().unwrap();
        let again = Footprints::load(dir.path());
        assert_eq!(again.get("shop", None).unwrap().1, 5);
    }

    #[test]
    fn gpus_are_not_measured() {
        let dir = tempfile::tempdir().unwrap();
        let f = Footprints::load(dir.path());
        let mut r = run("a", GIB, GIB);
        r.peak.gpus = Some(crate::share::GpuNeed {
            count: 2,
            memory: GIB,
        });
        f.record(&r, 1);
        assert_eq!(f.get("shop", None).unwrap().0.gpus, None);
    }
}
