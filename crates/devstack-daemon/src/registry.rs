//! The registry of stacks the hub knows (ADR-0021): every daemon registers its
//! stack when it starts, one file per stack next to the user's token, so
//! daemons starting at the same time never write the same file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Absolute path of the stack file.
    pub file: PathBuf,
    pub repo: PathBuf,
}

pub fn dir() -> PathBuf {
    crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .join("stacks")
}

/// Stable id of a stack: from its stack file's absolute path (case-folded on
/// Windows, where paths are case-insensitive).
pub fn id_for(file: &Path) -> String {
    let mut s = file.to_string_lossy().into_owned();
    if cfg!(windows) {
        s = s.to_lowercase().replace('/', "\\");
    }
    blake3::hash(s.as_bytes()).to_hex()[..12].to_string()
}

pub fn register(entry: &Entry) -> std::io::Result<()> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let tmp = d.join(format!("{}.json.tmp", entry.id));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(entry).expect("entry serialises"),
    )?;
    std::fs::rename(tmp, d.join(format!("{}.json", entry.id)))
}

pub fn list() -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    out
}

pub fn forget(id: &str) -> std::io::Result<()> {
    if !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(());
    }
    match std::fs::remove_file(dir().join(format!("{id}.json"))) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_stable_and_case_insensitive_on_windows() {
        let a = super::id_for(std::path::Path::new("C:\\Work\\Shop\\devstack.toml"));
        assert_eq!(a.len(), 12);
        assert_eq!(
            a,
            super::id_for(std::path::Path::new("C:\\Work\\Shop\\devstack.toml"))
        );
        if cfg!(windows) {
            assert_eq!(
                a,
                super::id_for(std::path::Path::new("c:/work/shop/devstack.toml"))
            );
        }
    }
}
