//! Resource usage (plan §12): sampled in the background, the last ten minutes
//! kept in memory for the dashboard's sparklines, nothing persisted.
//!
//! - native service trees and the build job every 2 s (`devstack-proc`),
//! - containers every 5 s (`docker stats`, which itself measures for ~2 s),
//! - disk under `.devstack/` every minute, Compose volumes every five.
//!
//! Builds are accounted separately from the services they belong to, so a
//! running build shows as such and not as the service's load.

use crate::state::{EventBody, now_ms};
use crate::supervisor::Supervisor;
use devstack_config::model::Member;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const NATIVE_EVERY: Duration = Duration::from_secs(2);
pub const CONTAINER_EVERY: Duration = Duration::from_secs(5);
const DISK_EVERY: Duration = Duration::from_secs(60);
const VOLUMES_EVERY: Duration = Duration::from_secs(300);
const KEEP: Duration = Duration::from_secs(600);

/// Load at one moment: CPU in percent of one core (may exceed 100), memory in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Load {
    pub cpu: f64,
    pub memory: u64,
}

impl Load {
    fn add(&mut self, o: Load) {
        self.cpu += o.cpu;
        self.memory += o.memory;
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Point {
    pub at: u128,
    pub cpu: f64,
    pub memory: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    #[serde(flatten)]
    pub load: Load,
    /// Native trees only: CPU time so far and process count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Point>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerEntry {
    pub group: String,
    pub service: String,
    #[serde(flatten)]
    pub load: Load,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Point>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupEntry {
    pub name: String,
    #[serde(flatten)]
    pub load: Load,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Disk {
    /// Bytes under the lane (snapshot), the artifact store and the logs.
    pub snapshot: u64,
    pub artifacts: u64,
    pub logs: u64,
    /// Named volumes of the lane's Compose projects; `None` until measured.
    pub volumes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LaneEntry {
    pub name: String,
    /// Services, builds and containers of the lane together.
    #[serde(flatten)]
    pub load: Load,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<Disk>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub at: u128,
    pub services: Vec<Entry>,
    pub builds: Entry,
    pub containers: Vec<ContainerEntry>,
    pub groups: Vec<GroupEntry>,
    pub lanes: Vec<LaneEntry>,
    /// Why container numbers are missing (Docker not reachable, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_error: Option<String>,
}

#[derive(Default)]
struct Series {
    /// Previous CPU-time reading of a native tree: (when, cpu_us).
    last_cpu: Option<(Instant, u64)>,
    current: Option<Load>,
    cpu_time_us: Option<u64>,
    processes: Option<u32>,
    history: VecDeque<Point>,
}

impl Series {
    fn push(&mut self, load: Load) {
        self.current = Some(load);
        let at = now_ms();
        self.history.push_back(Point {
            at,
            cpu: load.cpu,
            memory: load.memory,
        });
        let keep = KEEP.as_millis();
        while self
            .history
            .front()
            .is_some_and(|p| at.saturating_sub(p.at) > keep)
        {
            self.history.pop_front();
        }
    }

    fn native(&mut self, u: devstack_proc::Usage) {
        let now = Instant::now();
        let cpu = match self.last_cpu {
            // A drop in CPU time is a new instance (or a process-sum that lost
            // a process): no rate this round, start over from here.
            Some((t, prev)) if u.cpu_us >= prev => {
                let wall = now.duration_since(t).as_micros().max(1) as f64;
                (u.cpu_us - prev) as f64 / wall * 100.0
            }
            _ => 0.0,
        };
        self.last_cpu = Some((now, u.cpu_us));
        self.cpu_time_us = Some(u.cpu_us);
        self.processes = Some(u.processes);
        self.push(Load {
            cpu,
            memory: u.memory,
        });
    }

    /// Not running any more: no current load, history stays.
    fn idle(&mut self) {
        self.last_cpu = None;
        self.current = None;
        self.cpu_time_us = None;
        self.processes = None;
    }

    fn load(&self) -> Load {
        self.current.unwrap_or_default()
    }

    fn history(&self, with: bool) -> Vec<Point> {
        if with {
            self.history.iter().copied().collect()
        } else {
            Vec::new()
        }
    }
}

#[derive(Default)]
struct Inner {
    /// `service:<name>`, `build`, `container:<group>/<service>`.
    series: BTreeMap<String, Series>,
    disk: Disk,
    disk_measured: bool,
    docker_error: Option<String>,
}

pub struct Sampler {
    inner: Mutex<Inner>,
}

fn container_key(group: &str, service: &str) -> String {
    format!("container:{group}/{service}")
}

impl Sampler {
    /// Start the sampling threads. They end with the daemon process, or when
    /// the supervisor is closed (a removed lane).
    pub fn start(sup: Arc<Supervisor>) -> Arc<Self> {
        let me = Arc::new(Self {
            inner: Mutex::new(Inner::default()),
        });
        let (s, m) = (sup.clone(), me.clone());
        spawn("usage-native", move || {
            while !s.closed() {
                m.sample_native(&s);
                s.emit(EventBody::Usage(Box::new(m.report(&s, false))));
                std::thread::sleep(NATIVE_EVERY);
            }
        });
        let (s, m) = (sup.clone(), me.clone());
        spawn("usage-containers", move || {
            let mut volumes_due = Instant::now();
            while !s.closed() {
                let t0 = Instant::now();
                m.sample_containers(&s);
                if Instant::now() >= volumes_due {
                    m.sample_volumes(&s);
                    volumes_due = Instant::now() + VOLUMES_EVERY;
                }
                std::thread::sleep(CONTAINER_EVERY.saturating_sub(t0.elapsed()));
            }
        });
        let (s, m) = (sup, me.clone());
        spawn("usage-disk", move || {
            while !s.closed() {
                m.sample_disk(&s);
                std::thread::sleep(DISK_EVERY);
            }
        });
        me
    }

    fn sample_native(&self, sup: &Supervisor) {
        let trees: BTreeMap<String, devstack_proc::Usage> = sup.tree_usage().into_iter().collect();
        let build = sup.build_usage();
        let mut inner = self.inner.lock().unwrap();
        for name in sup.stack.services.keys() {
            let s = inner.series.entry(format!("service:{name}")).or_default();
            match trees.get(name) {
                Some(u) => s.native(*u),
                None => s.idle(),
            }
        }
        inner
            .series
            .entry("build".into())
            .or_default()
            .native(build);
    }

    fn sample_containers(&self, sup: &Supervisor) {
        // The container states themselves, so `status` and the dashboard see a
        // stopped or unhealthy container (they were read only at up/down before).
        sup.refresh_containers();
        let mut running = BTreeMap::new();
        for g in sup.view().infra {
            for c in g.containers {
                if c.state == "running" {
                    running.insert(c.name, (g.group.clone(), c.service));
                }
            }
        }
        if let Some(file) = sup.external_infra() {
            return self.sample_external(&file);
        }
        let names: Vec<String> = running.keys().cloned().collect();
        if names.is_empty() {
            return;
        }
        let stats = sup.docker().stats(&names);
        let mut inner = self.inner.lock().unwrap();
        let mut seen = Vec::new();
        match stats {
            Ok(stats) => {
                inner.docker_error = None;
                for st in stats {
                    if let Some((group, service)) = running.get(&st.name) {
                        let key = container_key(group, service);
                        inner.series.entry(key.clone()).or_default().push(Load {
                            cpu: st.cpu,
                            memory: st.memory,
                        });
                        seen.push(key);
                    }
                }
            }
            Err(e) => inner.docker_error = Some(e.to_string()),
        }
        for (key, s) in inner.series.iter_mut() {
            if key.starts_with("container:") && !seen.contains(key) {
                s.idle();
            }
        }
    }

    /// A lane on a node has no Docker: the node's agent measures its
    /// containers and writes what each uses into the file that describes
    /// them (#101). A measurement older than a few of its rounds is none.
    fn sample_external(&self, file: &std::path::Path) {
        const FRESH_MS: u64 = 20_000;
        let ext = crate::external::read(file).unwrap_or_default();
        let now = crate::state::now_ms() as u64;
        let fresh = ext.at.is_some_and(|at| now.saturating_sub(at) < FRESH_MS);
        let mut inner = self.inner.lock().unwrap();
        let mut seen = Vec::new();
        if fresh {
            for (group, g) in &ext.groups {
                for (service, s) in &g.services {
                    if let (true, Some(cpu), Some(memory)) = (s.state == "running", s.cpu, s.memory)
                    {
                        let key = container_key(group, service);
                        inner
                            .series
                            .entry(key.clone())
                            .or_default()
                            .push(Load { cpu, memory });
                        seen.push(key);
                    }
                }
            }
        }
        for (key, s) in inner.series.iter_mut() {
            if key.starts_with("container:") && !seen.contains(key) {
                s.idle();
            }
        }
    }

    fn sample_volumes(&self, sup: &Supervisor) {
        let projects: Vec<String> = sup.view().infra.into_iter().map(|g| g.project).collect();
        if projects.is_empty() || sup.external_infra().is_some() {
            return;
        }
        if let Ok(vols) = sup.docker().volumes() {
            let total = vols
                .iter()
                .filter(|v| v.project.as_ref().is_some_and(|p| projects.contains(p)))
                .map(|v| v.size)
                .sum();
            self.inner.lock().unwrap().disk.volumes = Some(total);
        }
    }

    fn sample_disk(&self, sup: &Supervisor) {
        let layout = sup.layout();
        let snapshot = dir_size(&layout.lane("main"));
        let artifacts = dir_size(&layout.state.join("artifacts"));
        let logs = dir_size(&layout.state.join("logs"));
        let mut inner = self.inner.lock().unwrap();
        inner.disk.snapshot = snapshot;
        inner.disk.artifacts = artifacts;
        inner.disk.logs = logs;
        inner.disk_measured = true;
    }

    /// The current picture; with `history`, the last ten minutes per series.
    pub fn report(&self, sup: &Supervisor, history: bool) -> UsageReport {
        let inner = self.inner.lock().unwrap();
        let empty = Series::default();
        let get = |key: &str| inner.series.get(key).unwrap_or(&empty);

        let services: Vec<Entry> = sup
            .stack
            .services
            .keys()
            .map(|name| {
                let s = get(&format!("service:{name}"));
                Entry {
                    name: name.clone(),
                    load: s.load(),
                    cpu_time_ms: s.cpu_time_us.map(|u| u / 1000),
                    processes: s.processes,
                    history: s.history(history),
                }
            })
            .collect();
        let b = get("build");
        let builds = Entry {
            name: "builds".into(),
            load: b.load(),
            cpu_time_ms: b.cpu_time_us.map(|u| u / 1000),
            processes: b.processes,
            history: b.history(history),
        };
        let containers: Vec<ContainerEntry> = inner
            .series
            .iter()
            .filter_map(|(key, s)| {
                let (group, service) = key.strip_prefix("container:")?.split_once('/')?;
                Some(ContainerEntry {
                    group: group.into(),
                    service: service.into(),
                    load: s.load(),
                    history: s.history(history),
                })
            })
            .collect();
        let groups = sup
            .stack
            .groups
            .iter()
            .map(|(name, g)| {
                let mut load = Load::default();
                for m in g.parsed_members().into_iter().flatten() {
                    let key = match m {
                        Member::Service(s) => format!("service:{s}"),
                        Member::ComposeService { group, service } => container_key(group, service),
                        Member::Command(_) => continue,
                    };
                    load.add(get(&key).load());
                }
                GroupEntry {
                    name: name.clone(),
                    load,
                }
            })
            .collect();
        // One sampler per supervisor: this report is its lane's (`main` for
        // the main working tree, ADR-0024); its builds are the daemon's one
        // build job (ADR-0020).
        let mut lane = Load::default();
        services.iter().for_each(|e| lane.add(e.load));
        lane.add(builds.load);
        containers.iter().for_each(|c| lane.add(c.load));
        UsageReport {
            at: now_ms(),
            services,
            builds,
            containers,
            groups,
            lanes: vec![LaneEntry {
                name: if sup.lane.is_empty() {
                    "main".into()
                } else {
                    sup.lane.clone()
                },
                load: lane,
                disk: inner.disk_measured.then(|| inner.disk.clone()),
            }],
            docker_error: inner.docker_error.clone(),
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("start a usage thread");
}

/// Bytes below `path`, not following links or junctions.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(e.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_rate_from_two_readings_and_a_restart() {
        let mut s = Series::default();
        let u = |cpu_us| devstack_proc::Usage {
            cpu_us,
            memory: 10,
            processes: 1,
        };
        s.native(u(1_000_000));
        assert_eq!(s.load().cpu, 0.0, "no rate from one reading");
        s.last_cpu = Some((Instant::now() - Duration::from_secs(2), 1_000_000));
        s.native(u(2_000_000));
        let cpu = s.load().cpu;
        assert!((45.0..=55.0).contains(&cpu), "1 s of CPU in 2 s: {cpu}");
        s.native(u(10)); // a new instance
        assert_eq!(s.load().cpu, 0.0);
        assert_eq!(s.history.len(), 3);
        s.idle();
        assert_eq!(s.load(), Load::default());
        assert_eq!(s.history.len(), 3, "history survives a stop");
    }

    #[test]
    fn disk_size_counts_files_below() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/x"), [0u8; 1000]).unwrap();
        std::fs::write(d.path().join("y"), [0u8; 24]).unwrap();
        assert_eq!(dir_size(d.path()), 1024);
        assert_eq!(dir_size(&d.path().join("missing")), 0);
    }
}
