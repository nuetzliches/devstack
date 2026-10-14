//! Snapshot of a lane's source (ADR-0002), the prototype of spike (c) made
//! durable: tracked plus untracked non-ignored files are copied into the
//! lane's `src`; afterwards only files whose size or mtime changed. Copies get
//! the current time as mtime, so incremental build tools see them as newer
//! than their outputs (a restored file keeps an old mtime otherwise). The
//! manifest keeps a content hash per file for build ids.

use crate::{BuildError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub size: u64,
    /// Source mtime in nanoseconds since the epoch.
    pub mtime: u128,
    /// blake3 of the content, hex.
    pub hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Repository-relative path with `/` separators.
    pub files: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStats {
    pub files: usize,
    pub copied: usize,
    pub removed: usize,
    pub elapsed: Duration,
}

pub struct Snapshot {
    pub source: PathBuf,
    /// Where the copy lives: `<state>/lanes/<lane>/src`.
    pub dir: PathBuf,
    manifest_path: PathBuf,
    pub manifest: Manifest,
}

/// Directories that are never part of a snapshot, even if the downstream
/// project does not ignore them: devstack's own state.
const EXCLUDED_PREFIXES: [&str; 1] = [".devstack/"];

impl Snapshot {
    pub fn open(source: &Path, lane_dir: &Path) -> Result<Self> {
        let manifest_path = lane_dir.join("manifest.json");
        let manifest = match fs::read(&manifest_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Manifest::default(),
        };
        Ok(Self {
            source: source.into(),
            dir: lane_dir.join("src"),
            manifest_path,
            manifest,
        })
    }

    /// Tracked and untracked, non-ignored files of the source (needs git).
    pub fn list(&self) -> Result<Vec<String>> {
        let out = std::process::Command::new("git")
            .args(["ls-files", "-co", "--exclude-standard", "-z"])
            .current_dir(&self.source)
            .output()
            .map_err(|e| BuildError::io("run git ls-files", e))?;
        if !out.status.success() {
            return Err(BuildError::Other(format!(
                "git ls-files failed in {}: {}",
                self.source.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| !p.is_empty() && !EXCLUDED_PREFIXES.iter().any(|x| p.starts_with(x)))
            .map(str::to_string)
            .collect())
    }

    pub fn sync(&mut self) -> Result<SyncStats> {
        let t0 = Instant::now();
        let files = self.list()?;
        let mut next = BTreeMap::new();
        let (mut copied, mut removed) = (0, 0);
        let now = SystemTime::now();
        for rel in &files {
            let src = self.source.join(rel);
            let Ok(meta) = fs::metadata(&src) else {
                continue;
            }; // deleted between list and stat
            if !meta.is_file() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            let unchanged = self.manifest.files.get(rel).filter(|e| {
                e.size == meta.len() && e.mtime == mtime && self.dir.join(rel).exists()
            });
            let entry = match unchanged {
                Some(e) => e.clone(),
                None => {
                    let bytes = fs::read(&src)
                        .map_err(|e| BuildError::io(format!("read {}", src.display()), e))?;
                    let dst = self.dir.join(rel);
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            BuildError::io(format!("create {}", parent.display()), e)
                        })?;
                    }
                    fs::write(&dst, &bytes)
                        .map_err(|e| BuildError::io(format!("write {}", dst.display()), e))?;
                    let f = fs::File::options()
                        .write(true)
                        .open(&dst)
                        .map_err(|e| BuildError::io("open copy", e))?;
                    f.set_modified(now)
                        .map_err(|e| BuildError::io("stamp copy", e))?;
                    copied += 1;
                    Entry {
                        size: meta.len(),
                        mtime,
                        hash: blake3::hash(&bytes).to_hex().to_string(),
                    }
                }
            };
            next.insert(rel.clone(), entry);
        }
        for gone in self
            .manifest
            .files
            .keys()
            .filter(|k| !next.contains_key(*k))
        {
            let _ = fs::remove_file(self.dir.join(gone));
            removed += 1;
        }
        self.manifest.files = next;
        if let Some(parent) = self.manifest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| BuildError::io("create lane dir", e))?;
        }
        fs::write(
            &self.manifest_path,
            serde_json::to_vec(&self.manifest).expect("manifest serialises"),
        )
        .map_err(|e| BuildError::io("write manifest", e))?;
        Ok(SyncStats {
            files: self.manifest.files.len(),
            copied,
            removed,
            elapsed: t0.elapsed(),
        })
    }
}
