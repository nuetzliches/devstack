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

/// A GPU of a machine (ADR-0036): reported, not placed yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    /// Its memory, and what of it is used now, bytes.
    pub memory: u64,
    pub memory_used: u64,
    /// Its load now, percent.
    pub load: f64,
    /// Whether the machine offers it to the pool.
    #[serde(default)]
    pub offered: bool,
}

/// The machine's GPUs, as the vendor's tool reports them (`nvidia-smi`, on
/// Linux and Windows); none without one.
pub fn gpus() -> Vec<Gpu> {
    let Ok(out) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout))
}

/// `name, memory.total, memory.used, utilization.gpu` per line, MiB and
/// percent.
pub fn parse_nvidia_smi(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            let [name, total, used, load] = f.as_slice() else {
                return None;
            };
            let mib = |x: &str| x.parse::<u64>().ok().map(|m| m << 20);
            Some(Gpu {
                name: (*name).to_string(),
                memory: mib(total)?,
                memory_used: mib(used).unwrap_or(0),
                load: load.parse().unwrap_or(0.0),
                offered: false,
            })
        })
        .collect()
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
    fn gpus_are_read_from_nvidia_smi() {
        let g = parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564, 1234, 7\nbroken line\n");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(g[0].memory, 24564 << 20);
        assert_eq!(g[0].memory_used, 1234 << 20);
        assert_eq!(g[0].load, 7.0);
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
    fn a_floor_raises_each_part_and_the_disk_in_all() {
        let measured = Need {
            cpus: 1.0,
            memory: 2 * GIB,
            disk_repo: 6 * GIB,
            disk_lane: 3 * GIB,
            gpus: None,
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
        };
        s.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()), s);
    }
}
