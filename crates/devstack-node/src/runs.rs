//! What each lease's lane takes on a node, measured while it runs
//! (ADR-0048 point 2): the peak of its memory, of its CPU averaged over a
//! minute, of its slot (mirror, caches, builds, volumes) and of the images
//! its containers run. The node reports the runs with its report, those
//! that ended once more after their end, and the coordinator keeps the
//! footprint. A run counts once its lane ran for [`COUNTS_AFTER`]: a start
//! that failed, or one just begun, would make the footprint smaller than
//! what the lane takes. Kept in memory: after a restart of the agent a
//! run's peak starts again, and the coordinator keeps the larger.

use crate::footprint::Run;
use crate::share::{Amount, Need};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Rounds a CPU reading is averaged over: about a minute at one round
/// every 15 s, so a build's minutes count and a single sample does not.
const CPU_WINDOW: usize = 4;

/// How long a lane runs before its run counts.
pub const COUNTS_AFTER: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
struct State {
    /// When its containers were first measured: the lane runs since.
    since: Option<Instant>,
    repo: String,
    profile: Option<String>,
    peak: Need,
    cpu: VecDeque<f64>,
    /// Its slot and its containers' images, as last measured.
    lane: u64,
    images: u64,
}

#[derive(Debug, Default)]
pub struct Runs {
    by: HashMap<String, State>,
    /// Runs that ended and are not reported since.
    ended: Vec<Run>,
}

impl Runs {
    /// Lease `id` of `repo` begins.
    pub fn begin(&mut self, id: &str, repo: &str) {
        self.by.entry(id.to_string()).or_insert_with(|| State {
            since: None,
            repo: repo.to_string(),
            profile: None,
            peak: Need::default(),
            cpu: VecDeque::new(),
            lane: 0,
            images: 0,
        });
    }

    /// It runs with `profile` (ADR-0041).
    pub fn profile(&mut self, id: &str, profile: Option<String>) {
        if let Some(s) = self.by.get_mut(id) {
            s.profile = profile;
        }
    }

    /// What it uses in this round.
    pub fn load(&mut self, id: &str, used: Amount) {
        let Some(s) = self.by.get_mut(id) else {
            return;
        };
        s.since.get_or_insert_with(Instant::now);
        s.peak.memory = s.peak.memory.max(used.memory);
        s.cpu.push_back(used.cpus);
        if s.cpu.len() > CPU_WINDOW {
            s.cpu.pop_front();
        }
        if s.cpu.len() == CPU_WINDOW {
            let avg = s.cpu.iter().sum::<f64>() / CPU_WINDOW as f64;
            s.peak.cpus = s.peak.cpus.max(avg);
        }
    }

    /// What its slot and its containers' images take now.
    pub fn disk(&mut self, id: &str, lane: u64, images: u64) {
        let Some(s) = self.by.get_mut(id) else {
            return;
        };
        s.lane = lane;
        s.images = images;
        s.peak.disk_lane = s.peak.disk_lane.max(lane);
        s.peak.disk_repo = s.peak.disk_repo.max(images);
    }

    /// What it took of the disk as last measured: its slot, its images.
    pub fn written(&self, id: &str) -> (u64, u64) {
        self.by.get(id).map_or((0, 0), |s| (s.lane, s.images))
    }

    /// It ended: reported once more if it counts, then forgotten.
    pub fn end(&mut self, id: &str) {
        if let Some(s) = self.by.remove(id)
            && counts(&s)
        {
            self.ended.push(run(id, s));
        }
    }

    /// The runs to report: those going on that count, and those that
    /// ended since.
    pub fn report(&self) -> Vec<Run> {
        self.by
            .iter()
            .filter(|(_, s)| counts(s))
            .map(|(id, s)| run(id, s.clone()))
            .chain(self.ended.iter().cloned())
            .collect()
    }

    /// The coordinator took `sent`: the ended ones among them go.
    pub fn reported(&mut self, sent: &[Run]) {
        self.ended.retain(|r| !sent.iter().any(|x| x.id == r.id));
    }
}

fn counts(s: &State) -> bool {
    s.since.is_some_and(|t| t.elapsed() >= COUNTS_AFTER)
}

fn run(id: &str, s: State) -> Run {
    Run {
        id: id.to_string(),
        repo: s.repo,
        profile: s.profile,
        peak: s.peak,
    }
}

/// What the containers of each lease use now (`docker stats`): `leases`
/// are each lease's id with the names its containers carry (the lane
/// container its id, its infrastructure its slot's, ADR-0038).
pub fn use_by(leases: &[(String, Vec<String>)]) -> HashMap<String, Amount> {
    let mut out = HashMap::new();
    if leases.is_empty() {
        return out;
    }
    let names = containers();
    let owned: Vec<(String, &str)> = names
        .iter()
        .filter_map(|(n, _)| Some((n.clone(), owner(leases, n)?)))
        .collect();
    if owned.is_empty() {
        return out;
    }
    let all: Vec<String> = owned.iter().map(|(n, _)| n.clone()).collect();
    for st in devstack_compose::Docker::default()
        .stats(&all)
        .unwrap_or_default()
    {
        let Some((_, id)) = owned.iter().find(|(n, _)| *n == st.name) else {
            continue;
        };
        let a: &mut Amount = out.entry((*id).to_string()).or_default();
        a.cpus += st.cpu / 100.0;
        a.memory += st.memory;
    }
    out
}

/// The bytes the images of each lease's containers take (their logical
/// size: layers two images share count in each).
pub fn images_by(leases: &[(String, Vec<String>)]) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    if leases.is_empty() {
        return out;
    }
    let mut by_lease: HashMap<&str, Vec<String>> = HashMap::new();
    for (name, image) in containers_all() {
        if let Some(id) = owner(leases, &name) {
            let v = by_lease.entry(id).or_default();
            if !v.contains(&image) {
                v.push(image);
            }
        }
    }
    let mut images: Vec<&String> = by_lease.values().flatten().collect();
    images.sort_unstable();
    images.dedup();
    let sizes = image_sizes(&images);
    for (id, imgs) in by_lease {
        let total = imgs.iter().filter_map(|i| sizes.get(i)).sum();
        out.insert(id.to_string(), total);
    }
    out
}

/// The lease whose container `name` is.
fn owner<'a>(leases: &'a [(String, Vec<String>)], name: &str) -> Option<&'a str> {
    leases
        .iter()
        .find(|(_, keys)| {
            keys.iter()
                .any(|k| !k.is_empty() && name.contains(k.as_str()))
        })
        .map(|(id, _)| id.as_str())
}

/// Running containers: name and image.
fn containers() -> Vec<(String, String)> {
    ps(&["ps", "--format", "{{.Names}}\t{{.Image}}"])
}

/// Every container, stopped ones too: name and image.
fn containers_all() -> Vec<(String, String)> {
    ps(&["ps", "-a", "--format", "{{.Names}}\t{{.Image}}"])
}

fn ps(args: &[&str]) -> Vec<(String, String)> {
    let Ok(out) = std::process::Command::new("docker").args(args).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (n, i) = l.trim_end_matches('\r').split_once('\t')?;
            Some((n.trim().to_string(), i.trim().to_string()))
        })
        .collect()
}

/// Each image's size, bytes (`docker image inspect`).
fn image_sizes(images: &[&String]) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    if images.is_empty() {
        return out;
    }
    let mut args = vec!["image", "inspect", "--format", "{{.Size}}", "--"];
    args.extend(images.iter().map(|s| s.as_str()));
    let Ok(o) = std::process::Command::new("docker").args(&args).output() else {
        return out;
    };
    // One line per image, in order; an image that is gone fails the whole
    // call, then each is asked alone.
    let lines: Vec<u64> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    if o.status.success() && lines.len() == images.len() {
        for (i, n) in images.iter().zip(lines) {
            out.insert((*i).clone(), n);
        }
        return out;
    }
    for i in images {
        if let Ok(o) = std::process::Command::new("docker")
            .args(["image", "inspect", "--format", "{{.Size}}", "--", i])
            .output()
            && let Ok(n) = String::from_utf8_lossy(&o.stdout).trim().parse()
        {
            out.insert((*i).clone(), n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn peaks_over_a_run_cpu_over_a_minute() {
        let mut r = Runs::default();
        r.begin("l1", "shop");
        r.profile("l1", Some("acceptance".into()));
        // A single spike does not count; four rounds at two cores do.
        for cpus in [8.0, 0.0, 0.0, 0.0] {
            r.load("l1", Amount { cpus, memory: GIB });
        }
        assert_eq!(r.by["l1"].peak.cpus, 2.0);
        for _ in 0..4 {
            r.load(
                "l1",
                Amount {
                    cpus: 2.0,
                    memory: 3 * GIB,
                },
            );
        }
        r.load(
            "l1",
            Amount {
                cpus: 0.1,
                memory: GIB,
            },
        );
        assert!(r.report().is_empty(), "not running for long enough yet");
        r.by.get_mut("l1").unwrap().since = Instant::now().checked_sub(COUNTS_AFTER);
        r.disk("l1", 4 * GIB, 2 * GIB);
        r.disk("l1", 3 * GIB, 2 * GIB);
        assert_eq!(r.written("l1"), (3 * GIB, 2 * GIB));
        let p = r.report()[0].peak;
        assert_eq!(
            (p.cpus, p.memory, p.disk_lane, p.disk_repo),
            (2.0, 3 * GIB, 4 * GIB, 2 * GIB)
        );
        // Ended: reported once more, then gone.
        r.end("l1");
        let sent = r.report();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].profile.as_deref(), Some("acceptance"));
        r.reported(&sent);
        assert!(r.report().is_empty());
        assert_eq!(r.written("l1"), (0, 0));
        // A lane that never ran long enough is not a run.
        r.begin("l2", "shop");
        r.load("l2", Amount::default());
        r.end("l2");
        assert!(r.report().is_empty());
    }

    #[test]
    fn a_container_belongs_to_the_lease_its_name_carries() {
        let leases = vec![
            (
                "aa11".to_string(),
                vec!["aa11".to_string(), "slot1".to_string()],
            ),
            ("bb22".to_string(), vec!["bb22".to_string(), String::new()]),
        ];
        assert_eq!(owner(&leases, "devstack-lane-aa11"), Some("aa11"));
        assert_eq!(owner(&leases, "shop-slot1-db-1"), Some("aa11"));
        assert_eq!(owner(&leases, "devstack-lane-bb22"), Some("bb22"));
        assert_eq!(owner(&leases, "other"), None);
    }
}
