//! What a machine shares with the pool (ADR-0031 point 3): with the switch on,
//! half of the memory and CPU that are free right now, never more than the
//! cap its owner sets. Kept in `share.json` next to the membership.

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

    /// What the machine offers now, given what is `free`: half of it, capped;
    /// nothing with the switch off.
    pub fn offer(&self, free: Amount) -> Amount {
        if !self.enabled {
            return Amount::default();
        }
        let cpus = free.cpus / 2.0;
        let memory = free.memory / 2;
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
        assert_eq!(off.offer(free), Amount::default(), "off shares nothing");
        let on = Settings {
            enabled: true,
            ..Settings::default()
        };
        assert_eq!(
            on.offer(free),
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
            capped.offer(free),
            Amount {
                cpus: 2.0,
                memory: 4 * GIB
            }
        );
        assert_eq!(idle_cpus((100, 1000), (150, 1100), 8), 4.0);
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
