//! What a machine shares with the pool (ADR-0031 point 3): with the switch on,
//! half of the memory and CPU that are free right now (a node's policy may
//! say another part), never more than the cap its owner sets. Kept in `share.json` next to the membership.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The owner's choice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The switch: off by default, a machine shares only when its owner says so.
    #[serde(default)]
    pub enabled: bool,
    /// The most CPU cores it ever offers; `None`: no cap besides half of free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpus: Option<f64>,
    /// The most memory it ever offers, bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory: Option<u64>,
    /// The most GPUs its lanes hold together; `None`: every one, `0`: none
    /// (the owner keeps them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gpus: Option<u32>,
}

/// CPU cores and memory (bytes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Amount {
    pub cpus: f64,
    pub memory: u64,
}

/// What a lane needs (ADR-0049 point 1): CPU cores, memory, disk (bytes)
/// and GPUs. A superset of [`Amount`] on the wire: a peer that knows only
/// that reads the CPU and memory of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Need {
    pub cpus: f64,
    pub memory: u64,
    /// Of `memory`, the most its build job took (compilers and the build
    /// servers that stay warm after a build, ADR-0119 point 3): said, so a
    /// need can be explained; never added, `memory` holds it already. Absent
    /// from an older peer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub builds: u64,
    /// The repository's part of its disk: the lane image and the
    /// infrastructure's images, shared by the repository's lanes on a node.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disk_repo: u64,
    /// The lane's own part: its slot (mirror, caches, build output,
    /// volumes, ADR-0038).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disk_lane: u64,
    /// GPUs it asks for; declared only, never measured (ADR-0049 point 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<GpuNeed>,
}

/// GPUs a lane asks for: how many, each with at least `memory` bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuNeed {
    pub count: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub memory: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl Need {
    /// Its disk, both parts.
    pub fn disk(&self) -> u64 {
        self.disk_repo.saturating_add(self.disk_lane)
    }

    /// The disk it needs on a node beyond what is there already: nothing
    /// where the lane has a slot (`kept`), its own part where the
    /// repository has one (`warm`: its images are there), else both
    /// (ADR-0049 point 6).
    pub fn disk_on(&self, kept: bool, warm: bool) -> u64 {
        if kept {
            0
        } else if warm {
            self.disk_lane
        } else {
            self.disk()
        }
    }

    /// Each part the larger of the two.
    pub fn max(self, o: Need) -> Need {
        Need {
            cpus: self.cpus.max(o.cpus),
            memory: self.memory.max(o.memory),
            builds: self.builds.max(o.builds),
            disk_repo: self.disk_repo.max(o.disk_repo),
            disk_lane: self.disk_lane.max(o.disk_lane),
            gpus: match (self.gpus, o.gpus) {
                (Some(a), Some(b)) => Some(GpuNeed {
                    count: a.count.max(b.count),
                    memory: a.memory.max(b.memory),
                }),
                (a, b) => a.or(b),
            },
        }
    }

    /// Raised to `floor` (what a stack file declares, ADR-0049 point 5):
    /// CPU, memory and GPUs each at least the floor's, the disk at least
    /// the floor's in all, the difference added to the lane's part.
    /// Whether the floor raised anything is the second value.
    pub fn at_least(self, floor: Need) -> (Need, bool) {
        let mut n = Need {
            disk_repo: self.disk_repo,
            disk_lane: self.disk_lane,
            ..self.max(Need {
                disk_repo: 0,
                disk_lane: 0,
                ..floor
            })
        };
        if n.disk() < floor.disk() {
            n.disk_lane = floor.disk().saturating_sub(n.disk_repo);
        }
        (n, n != self)
    }

    /// CPU and memory alone.
    pub fn amount(&self) -> Amount {
        Amount {
            cpus: self.cpus,
            memory: self.memory,
        }
    }
}

impl From<Amount> for Need {
    fn from(a: Amount) -> Self {
        Need {
            cpus: a.cpus,
            memory: a.memory,
            ..Need::default()
        }
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join("share.json")
}

impl Settings {
    pub fn load(dir: &Path) -> Self {
        std::fs::read(path(dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            path(dir),
            serde_json::to_vec_pretty(self).expect("settings serialise"),
        )
    }

    /// The most CPU the machine offers its lanes, idle: `percent` of its
    /// `cores` (half when `None`), at most `max_cpus`. A run's measured CPU
    /// is capped at half of it (ADR-0066).
    pub fn most_cpus(&self, cores: f64, percent: Option<u8>) -> f64 {
        let part = f64::from(percent.unwrap_or(50).clamp(1, 100)) / 100.0;
        let cpus = cores * part;
        self.max_cpus.map_or(cpus, |m| cpus.min(m)).max(0.0)
    }

    /// What the machine offers now, given what is `free`: `percent` of it
    /// (half when `None`), capped; nothing with the switch off.
    pub fn offer(&self, free: Amount, percent: Option<u8>) -> Amount {
        if !self.enabled {
            return Amount::default();
        }
        let part = f64::from(percent.unwrap_or(50).clamp(1, 100)) / 100.0;
        let cpus = free.cpus * part;
        let memory = (free.memory as f64 * part) as u64;
        Amount {
            cpus: self.max_cpus.map_or(cpus, |m| cpus.min(m)).max(0.0),
            memory: self.max_memory.map_or(memory, |m| memory.min(m)),
        }
    }
}

/// A GPU of a machine (ADR-0036), and the lane it is assigned to on a node
/// (ADR-0112).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    /// The vendor's id (`GPU-…`), what a container is given; it stays on the
    /// machine (a report does not carry it).
    #[serde(skip)]
    pub id: String,
    pub name: String,
    /// Its memory, and what of it is used now, bytes.
    pub memory: u64,
    pub memory_used: u64,
    /// Its load now, percent.
    pub load: f64,
    /// Whether the machine offers it to the pool: free for a lane. A GPU a
    /// lane holds is not offered.
    #[serde(default)]
    pub offered: bool,
    /// The lane that holds it (`<repo>/<lane>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// Not offered because a process outside the pool's lanes uses it: more
    /// than [`IN_USE`] of its memory, with no lane holding it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
}

/// The part of a free GPU's memory that, used by other processes, keeps it
/// from the pool: its owner works on it.
pub const IN_USE: f64 = 0.25;

impl Gpu {
    /// Whether processes on the machine use more than [`IN_USE`] of its
    /// memory.
    pub fn in_use(&self) -> bool {
        self.memory > 0 && self.memory_used as f64 > self.memory as f64 * IN_USE
    }
}

/// How many more GPUs the node's lanes may hold: the owner's cap `max`
/// less the GPUs its leases hold (`held`, counted from the leases, not from
/// what the vendor's tool lists now); no cap: any number.
pub fn gpu_room(max: Option<u32>, held: usize) -> usize {
    max.map_or(usize::MAX, |m| (m as usize).saturating_sub(held))
}

/// The machine's GPUs as a node offers them (ADR-0112 point 5): one a lease
/// holds (`holder` names its lane) is not offered, nor one in use on the
/// machine; of the others, as many as `room` ([`gpu_room`]) leaves, the ones
/// with the most memory, so that the pool places on what fits best. The cap
/// is a number of GPUs: the admission ([`assign_gpus`]) may give any free one.
pub fn offered_gpus(
    gpus: Vec<Gpu>,
    holder: impl Fn(&str) -> Option<String>,
    room: usize,
) -> Vec<Gpu> {
    let lanes: Vec<Option<String>> = gpus.iter().map(|g| holder(&g.id)).collect();
    let free = |i: usize| lanes[i].is_none() && !gpus[i].in_use();
    // The free ones by memory, most first (a stable sort keeps the tool's
    // order among equals); the first `room` of them are offered.
    let mut order: Vec<usize> = (0..gpus.len()).filter(|&i| free(i)).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(gpus[i].memory));
    let offered: std::collections::HashSet<usize> = order.into_iter().take(room).collect();
    gpus.into_iter()
        .zip(lanes)
        .enumerate()
        .map(|(i, (g, lane))| Gpu {
            offered: offered.contains(&i),
            busy: lane.is_none() && g.in_use(),
            lane,
            ..g
        })
        .collect()
}

/// How long `nvidia-smi` may take. It hangs at times (a driver fault, an
/// Xid error), and its callers report every lane's containers too (review
/// before 0.52.0); it normally answers in well under a second.
const NVIDIA_SMI_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

/// The machine's GPUs, as the vendor's tool reports them (`nvidia-smi`, on
/// Linux and Windows); none without one, or when it does not answer within
/// [`NVIDIA_SMI_WITHIN`].
pub fn gpus() -> Vec<Gpu> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args([
        "--query-gpu=name,memory.total,memory.used,utilization.gpu,uuid",
        "--format=csv,noheader,nounits",
    ]);
    match devstack_compose::output_within(cmd, NVIDIA_SMI_WITHIN) {
        Ok(Some(out)) if out.status.success() => {
            parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

/// `name, memory.total, memory.used, utilization.gpu, uuid` per line, MiB
/// and percent.
pub fn parse_nvidia_smi(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            let [name, total, used, load, id] = f.as_slice() else {
                return None;
            };
            let mib = |x: &str| x.parse::<u64>().ok().map(|m| m << 20);
            Some(Gpu {
                id: (*id).to_string(),
                name: (*name).to_string(),
                memory: mib(total)?,
                memory_used: mib(used).unwrap_or(0),
                load: load.parse().unwrap_or(0.0),
                offered: false,
                lane: None,
                busy: false,
            })
        })
        .collect()
}

/// The GPUs a lane that needs `need` is given (ADR-0112 point 2): the first
/// free ones (held by no lease, not in use on the machine, as
/// [`offered_gpus`] marks them) with the memory it asks for, in the vendor
/// tool's order, as long as `room` ([`gpu_room`]) allows that many. `Err`:
/// how many it could have, fewer than it asks for.
pub fn assign_gpus(gpus: &[Gpu], need: GpuNeed, room: usize) -> Result<Vec<String>, usize> {
    let fit: Vec<String> = gpus
        .iter()
        .filter(|g| g.lane.is_none() && !g.busy && !g.id.is_empty() && g.memory >= need.memory)
        .map(|g| g.id.clone())
        .collect();
    let can = fit.len().min(room);
    if can < need.count as usize {
        return Err(can);
    }
    Ok(fit.into_iter().take(need.count as usize).collect())
}

/// The CPU cores idle between two readings of `devstack_proc::cpu_times`.
pub fn idle_cpus(before: (u64, u64), after: (u64, u64), cores: usize) -> f64 {
    let idle = after.0.saturating_sub(before.0) as f64;
    let total = after.1.saturating_sub(before.1) as f64;
    if total <= 0.0 {
        return 0.0;
    }
    (idle / total).clamp(0.0, 1.0) * cores as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_a_node_offers_is_its_part_of_its_cores_capped() {
        let s = Settings::default();
        assert_eq!(s.most_cpus(24.0, None), 12.0);
        assert!((s.most_cpus(24.0, Some(80)) - 19.2).abs() < 1e-9);
        let s = Settings {
            max_cpus: Some(16.0),
            ..Settings::default()
        };
        assert_eq!(s.most_cpus(24.0, Some(80)), 16.0);
        assert_eq!(s.most_cpus(24.0, None), 12.0);
    }

    #[test]
    fn gpus_are_read_from_nvidia_smi() {
        let g =
            parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564, 1234, 7, GPU-5b7c\nbroken line\n");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].id, "GPU-5b7c");
        assert_eq!(g[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(g[0].memory, 24564 << 20);
        assert_eq!(g[0].memory_used, 1234 << 20);
        assert_eq!(g[0].load, 7.0);
    }

    #[test]
    fn a_lane_is_given_free_gpus_with_the_memory_it_asks_for() {
        let gpu = |id: &str, gib: u64| Gpu {
            id: id.into(),
            memory: gib * GIB,
            ..Gpu::default()
        };
        let gpus = [gpu("a", 8), gpu("b", 24), gpu("c", 24)];
        let need = |count, gib: u64| GpuNeed {
            count,
            memory: gib * GIB,
        };
        // `held`: the GPUs the leases hold, `max` the owner's cap.
        let assign = |held: &[&str], max: Option<u32>, n: GpuNeed| {
            let ids: Vec<String> = held.iter().map(|h| h.to_string()).collect();
            let room = gpu_room(max, held.len());
            let view = offered_gpus(
                gpus.to_vec(),
                |id| ids.iter().any(|h| h == id).then(|| "shop/x".into()),
                room,
            );
            assign_gpus(&view, n, room)
        };
        assert_eq!(assign(&[], None, need(1, 0)), Ok(vec!["a".into()]));
        assert_eq!(assign(&[], None, need(1, 16)), Ok(vec!["b".into()]));
        assert_eq!(assign(&["b"], None, need(1, 16)), Ok(vec!["c".into()]));
        assert_eq!(assign(&["b"], None, need(2, 16)), Err(1));
        // The owner's cap counts what lanes hold already.
        assert_eq!(assign(&["b"], Some(1), need(1, 0)), Err(0));
        assert_eq!(assign(&[], Some(0), need(1, 0)), Err(0));
        // The cap is a number of GPUs: a lane gets one that fits, not the
        // first one (review before 0.52.0).
        assert_eq!(assign(&[], Some(1), need(1, 16)), Ok(vec!["b".into()]));
        // What a lease holds counts even when the tool does not list it now
        // (a GPU off the bus): no GPU beyond the cap.
        assert_eq!(assign(&["gone"], Some(1), need(1, 0)), Err(0));
        // A GPU whose id the tool did not say cannot be given to a container.
        let unnamed = offered_gpus(vec![gpu("", 24)], |_| None, usize::MAX);
        assert_eq!(assign_gpus(&unnamed, need(1, 0), usize::MAX), Err(0));
    }

    #[test]
    fn a_node_offers_no_gpu_beyond_its_cap_or_in_use_on_the_machine() {
        let gpu = |id: &str, used_gib: u64| Gpu {
            id: id.into(),
            memory: 16 * GIB,
            memory_used: used_gib * GIB,
            ..Gpu::default()
        };
        // `b`: a quarter of its memory is still free to take; `c` is the
        // owner's training.
        let gpus = vec![gpu("a", 0), gpu("b", 4), gpu("c", 5), gpu("d", 0)];
        let all = offered_gpus(
            gpus.clone(),
            |id| (id == "d").then(|| "shop/x".into()),
            usize::MAX,
        );
        let state: Vec<(bool, bool, bool)> = all
            .iter()
            .map(|g| (g.offered, g.busy, g.lane.is_some()))
            .collect();
        assert_eq!(
            state,
            [
                (true, false, false),
                (true, false, false),
                (false, true, false),
                (false, false, true)
            ]
        );
        // At most two: `d`, held, and one free one.
        let capped = offered_gpus(
            gpus,
            |id| (id == "d").then(|| "shop/x".into()),
            gpu_room(Some(2), 1),
        );
        assert_eq!(capped.iter().filter(|g| g.offered).count(), 1);
        // With room for fewer than are free, the largest are offered.
        let sized = |id: &str, gib: u64| Gpu {
            id: id.into(),
            memory: gib * GIB,
            ..Gpu::default()
        };
        let mixed = offered_gpus(vec![sized("s", 8), sized("l", 24)], |_| None, 1);
        let offered: Vec<&str> = mixed
            .iter()
            .filter(|g| g.offered)
            .map(|g| g.id.as_str())
            .collect();
        assert_eq!(offered, ["l"]);
        // A GPU a lane holds is never busy, whatever it uses.
        let held = offered_gpus(vec![gpu("a", 15)], |_| Some("shop/x".into()), usize::MAX);
        assert!(!held[0].busy && !held[0].offered);
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn half_of_free_never_above_the_cap() {
        let free = Amount {
            cpus: 10.0,
            memory: 20 * GIB,
        };
        let off = Settings::default();
        assert_eq!(
            off.offer(free, None),
            Amount::default(),
            "off shares nothing"
        );
        let on = Settings {
            enabled: true,
            ..Settings::default()
        };
        assert_eq!(
            on.offer(free, None),
            Amount {
                cpus: 5.0,
                memory: 10 * GIB
            }
        );
        let capped = Settings {
            enabled: true,
            max_cpus: Some(2.0),
            max_memory: Some(4 * GIB),
            max_gpus: None,
        };
        assert_eq!(
            capped.offer(free, None),
            Amount {
                cpus: 2.0,
                memory: 4 * GIB
            }
        );
        // A policy's part instead of half, still under the cap.
        assert_eq!(
            on.offer(free, Some(80)),
            Amount {
                cpus: 8.0,
                memory: 16 * GIB
            }
        );
        assert_eq!(capped.offer(free, Some(80)).cpus, 2.0);
        assert_eq!(on.offer(free, Some(0)).cpus, 0.1, "at least 1 percent");
        assert_eq!(idle_cpus((100, 1000), (150, 1100), 8), 4.0);
    }

    #[test]
    fn a_needs_builds_are_said_apart_and_an_older_peer_reads_without_them() {
        let n = Need {
            cpus: 1.0,
            memory: 9 * GIB,
            builds: 5 * GIB,
            ..Need::default()
        };
        let wire = serde_json::to_value(n).unwrap();
        assert_eq!(wire["builds"], 5 * GIB);
        // What an older peer sends has none; what it reads keeps its memory.
        let old: Need = serde_json::from_str(r#"{"cpus":1.0,"memory":1024}"#).unwrap();
        assert_eq!((old.memory, old.builds), (1024, 0));
        let amount: Amount = serde_json::from_value(wire).unwrap();
        assert_eq!(amount.memory, 9 * GIB);
        assert!(
            !serde_json::to_string(&Need::default())
                .unwrap()
                .contains("builds")
        );
        assert_eq!(old.max(n).builds, 5 * GIB);
    }

    #[test]
    fn a_floor_raises_each_part_and_the_disk_in_all() {
        let measured = Need {
            cpus: 1.0,
            memory: 2 * GIB,
            disk_repo: 6 * GIB,
            disk_lane: 3 * GIB,
            ..Need::default()
        };
        let (same, raised) = measured.at_least(Need {
            memory: GIB,
            disk_lane: 5 * GIB,
            ..Need::default()
        });
        assert_eq!(
            (same, raised),
            (measured, false),
            "below the measure: nothing"
        );
        let (n, raised) = measured.at_least(Need {
            cpus: 2.0,
            disk_lane: 20 * GIB,
            gpus: Some(GpuNeed {
                count: 1,
                memory: 16 * GIB,
            }),
            ..Need::default()
        });
        assert!(raised);
        assert_eq!((n.cpus, n.memory), (2.0, 2 * GIB));
        assert_eq!((n.disk_repo, n.disk_lane), (6 * GIB, 14 * GIB));
        assert_eq!(n.gpus.map(|g| g.count), Some(1));
        assert_eq!(n.disk_on(true, true), 0);
        assert_eq!(n.disk_on(false, true), 14 * GIB);
        assert_eq!(n.disk_on(false, false), 20 * GIB);
    }

    #[test]
    fn a_need_reads_as_an_amount_and_back() {
        let n: Need = serde_json::from_str(r#"{"cpus":0.5,"memory":1024}"#).unwrap();
        assert_eq!(
            n,
            Need::from(Amount {
                cpus: 0.5,
                memory: 1024
            })
        );
        let a: Amount =
            serde_json::from_str(&serde_json::to_string(&Need { disk_lane: 9, ..n }).unwrap())
                .unwrap();
        assert_eq!(a, n.amount());
    }

    #[test]
    fn settings_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
        let s = Settings {
            enabled: true,
            max_cpus: Some(3.0),
            max_memory: None,
            max_gpus: Some(0),
        };
        s.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()), s);
    }
}
