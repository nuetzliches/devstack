//! What each lease's lane takes on a node, measured while it runs
//! (ADR-0049 point 2): the peak of its memory, of its CPU averaged over a
//! minute, of its slot (mirror, caches, builds, volumes) and of the images
//! its containers run. The node reports the runs with its report, those
//! that ended once more after their end, and the coordinator keeps the
//! footprint. A run counts once its lane ran for [`COUNTS_AFTER`]: a start
//! that failed, or one just begun, would make the footprint smaller than
//! what the lane takes. Kept in memory: after a restart of the agent a
//! run's peak starts again, and the coordinator keeps the larger.
//!
//! Besides the peak, its steady use: the 95th percentile of its CPU (the
//! minute's average) and of its memory once the lane is up, so its build
//! does not count. Shown, never admitted by.

use crate::footprint::Run;
use crate::share::{Amount, Need};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Rounds a CPU reading is averaged over: about a minute at one round
/// every 15 s, so a build's minutes count and a single sample does not.
const CPU_WINDOW: usize = 4;

/// How long a lane runs before its run counts.
pub const COUNTS_AFTER: Duration = Duration::from_secs(180);

/// The steps steady use is counted in: a tenth of a core, 64 MiB.
const CPU_STEP: f64 = 0.1;
const MEMORY_STEP: u64 = 64 << 20;

/// Readings counted by step, so a lane that runs for weeks keeps a few
/// hundred numbers, not every reading.
#[derive(Debug, Clone, Default)]
struct Steps {
    by: BTreeMap<u64, u32>,
    n: u32,
}

impl Steps {
    /// A reading of `steps` (rounded up: a step is its upper bound).
    fn add(&mut self, steps: u64) {
        *self.by.entry(steps).or_default() += 1;
        self.n = self.n.saturating_add(1);
    }

    /// The 95th percentile, in steps; `None` without readings.
    fn p95(&self) -> Option<u64> {
        let want = (self.n as f64 * 0.95).ceil() as u32;
        let mut seen = 0;
        self.by.iter().find_map(|(k, c)| {
            seen += c;
            (seen >= want && self.n > 0).then_some(*k)
        })
    }
}

#[derive(Debug, Clone)]
struct State {
    /// When its containers were first measured: the lane runs since.
    since: Option<Instant>,
    repo: String,
    profile: Option<String>,
    peak: Need,
    cpu: VecDeque<f64>,
    /// The lane is up (started, its build done): readings since count as
    /// steady use.
    up: bool,
    steady_cpu: Steps,
    steady_memory: Steps,
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
            up: false,
            steady_cpu: Steps::default(),
            steady_memory: Steps::default(),
            lane: 0,
            images: 0,
        });
    }

    /// Its lane is up: what it uses from now on is its steady use.
    pub fn up(&mut self, id: &str) {
        if let Some(s) = self.by.get_mut(id) {
            s.up = true;
            // A minute that holds the build's end is not steady.
            s.cpu.clear();
        }
    }

    /// It runs with `profile` (ADR-0041).
    pub fn profile(&mut self, id: &str, profile: Option<String>) {
        if let Some(s) = self.by.get_mut(id) {
            s.profile = profile;
        }
    }

    /// What it uses in this round. CPU counts up to `cpu_cap` (ADR-0066):
    /// a build takes what is free, more says how idle the node was.
    pub fn load(&mut self, id: &str, used: Amount, cpu_cap: f64) {
        let Some(s) = self.by.get_mut(id) else {
            return;
        };
        s.since.get_or_insert_with(Instant::now);
        s.peak.memory = s.peak.memory.max(used.memory);
        s.cpu.push_back(used.cpus.min(cpu_cap));
        if s.cpu.len() > CPU_WINDOW {
            s.cpu.pop_front();
        }
        if s.cpu.len() == CPU_WINDOW {
            let avg = s.cpu.iter().sum::<f64>() / CPU_WINDOW as f64;
            s.peak.cpus = s.peak.cpus.max(avg);
            if s.up {
                // Less a hair: 2.0 / 0.1 is 20.000000000000004.
                s.steady_cpu
                    .add((avg / CPU_STEP - 1e-9).ceil().max(0.0) as u64);
            }
        }
        if s.up {
            s.steady_memory.add(used.memory.div_ceil(MEMORY_STEP));
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
    let steady = match (s.steady_cpu.p95(), s.steady_memory.p95()) {
        (Some(c), Some(m)) => Some(Amount {
            cpus: c as f64 * CPU_STEP,
            memory: m * MEMORY_STEP,
        }),
        _ => None,
    };
    Run {
        id: id.to_string(),
        repo: s.repo,
        profile: s.profile,
        peak: s.peak,
        capped: true,
        steady,
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
            r.load("l1", Amount { cpus, memory: GIB }, 12.0);
        }
        assert_eq!(r.by["l1"].peak.cpus, 2.0);
        for _ in 0..4 {
            r.load(
                "l1",
                Amount {
                    cpus: 2.0,
                    memory: 3 * GIB,
                },
                12.0,
            );
        }
        r.load(
            "l1",
            Amount {
                cpus: 0.1,
                memory: GIB,
            },
            12.0,
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
        r.load("l2", Amount::default(), 12.0);
        r.end("l2");
        assert!(r.report().is_empty());
    }

    #[test]
    fn steady_use_counts_from_up_its_95th_percentile() {
        let mut r = Runs::default();
        r.begin("l1", "shop");
        let at = |cpus, memory| Amount { cpus, memory };
        // The build: 8 cores, 7 GiB; not steady use.
        for _ in 0..8 {
            r.load("l1", at(8.0, 7 * GIB), 12.0);
        }
        r.up("l1");
        r.by.get_mut("l1").unwrap().since = Instant::now().checked_sub(COUNTS_AFTER);
        for _ in 0..3 {
            r.load("l1", at(1.0, 2 * GIB), 12.0);
        }
        // The window still holds a build reading: no steady CPU yet.
        assert_eq!(r.report()[0].steady, None);
        // 100 rounds at a core and 2 GiB, a half-minute hot reload at 8.
        for i in 0..102 {
            let hot = (50..52).contains(&i);
            r.load("l1", at(if hot { 8.0 } else { 1.0 }, 2 * GIB), 12.0);
        }
        let run = &r.report()[0];
        assert_eq!(run.peak.cpus, 8.0);
        assert_eq!(run.peak.memory, 7 * GIB);
        let s = run.steady.unwrap();
        assert!((s.cpus - 1.0).abs() < 1e-9, "{}", s.cpus);
        assert_eq!(s.memory, 2 * GIB);
    }

    #[test]
    fn steps_p95() {
        let mut s = Steps::default();
        assert_eq!(s.p95(), None);
        for k in 1..=100 {
            s.add(k);
        }
        assert_eq!(s.p95(), Some(95));
    }

    #[test]
    fn a_build_that_takes_the_whole_node_needs_half_of_it() {
        let mut r = Runs::default();
        r.begin("l1", "shop");
        for _ in 0..4 {
            r.load(
                "l1",
                Amount {
                    cpus: 18.5,
                    memory: GIB,
                },
                10.0,
            );
        }
        assert_eq!(r.by["l1"].peak.cpus, 10.0);
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
