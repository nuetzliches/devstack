//! The registry of stacks the hub knows (ADR-0021): every daemon registers its
//! stack when it starts, one file per stack next to the user's token, so
//! daemons starting at the same time never write the same file.
//!
//! An entry also holds what is this machine's own of the stack (ADR-0082):
//! its port shift (`shift`, set by `devstack up --shift`) and the ports its
//! daemon claims (`claims`, written while it runs and kept after it stops).
//! Every write keeps what it does not set, fields of a newer devstack too.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Absolute path of the stack file.
    pub file: PathBuf,
    pub repo: PathBuf,
    /// What the main tree's ports are moved by on this machine (ADR-0082
    /// point 4): a multiple of [`SHIFT_STEP`]; 0, or absent, runs it on the
    /// ports its stack file gives.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shift: u16,
    /// The ports its daemon claims (ADR-0082 point 5).
    #[serde(default, skip_serializing_if = "Claims::is_empty")]
    pub claims: Claims,
}

fn is_zero(n: &u16) -> bool {
    *n == 0
}

/// What a shift is a multiple of.
pub const SHIFT_STEP: u16 = 1000;

/// The ports a stack claims on this machine: its main slot's after the
/// shift (services, held and published container ports, announced ports,
/// `urls`), and the range of each lane it numbered. A stopped stack keeps
/// them: whoever was first keeps its ports.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main: Vec<u16>,
    /// By lane name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<String, Vec<u16>>,
}

impl Claims {
    pub fn is_empty(&self) -> bool {
        self.main.is_empty() && self.lanes.is_empty()
    }

    /// Every port claimed, the lanes' too.
    pub fn ports(&self) -> BTreeSet<u16> {
        self.main
            .iter()
            .chain(self.lanes.values().flatten())
            .copied()
            .collect()
    }

    /// Who claims `port`: `main` or the lane's name.
    pub fn holder(&self, port: u16) -> Option<&str> {
        if self.main.contains(&port) {
            return Some("main");
        }
        self.lanes
            .iter()
            .find(|(_, ps)| ps.contains(&port))
            .map(|(l, _)| l.as_str())
    }
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

fn path_of(d: &Path, id: &str) -> PathBuf {
    d.join(format!("{id}.json"))
}

/// The entry file as it is, as JSON: what a write keeps.
fn read_raw(d: &Path, id: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_slice(&std::fs::read(path_of(d, id)).ok()?).ok()? {
        serde_json::Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Change the entry `id` in `d` by `f`, keeping every field `f` does not
/// touch. Written to a file of this process first, then renamed over the
/// entry: a reader never sees half of it, and two writers never share the
/// temporary file.
fn change(
    d: &Path,
    id: &str,
    f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> std::io::Result<()> {
    if !id.chars().all(|c| c.is_ascii_hexdigit()) || id.is_empty() {
        return Err(std::io::Error::other(format!("not a stack id: {id}")));
    }
    std::fs::create_dir_all(d)?;
    let mut m = read_raw(d, id).unwrap_or_default();
    f(&mut m);
    let tmp = d.join(format!("{id}.json.{}.tmp", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&serde_json::Value::Object(m)).expect("entry serialises"),
    )?;
    std::fs::rename(&tmp, path_of(d, id)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Register the stack: its id, name, stack file and repository. Its shift
/// and claims, and whatever else the entry holds, stay.
pub fn register(entry: &Entry) -> std::io::Result<()> {
    register_in(&dir(), entry)
}

fn register_in(d: &Path, entry: &Entry) -> std::io::Result<()> {
    let v = serde_json::to_value(entry).expect("entry serialises");
    change(d, &entry.id, |m| {
        for k in ["id", "name", "file", "repo"] {
            m.insert(k.into(), v[k].clone());
        }
    })
}

/// Set the shift of the stack `entry` (registered with it if it is not yet).
pub fn set_shift(entry: &Entry, shift: u16) -> std::io::Result<()> {
    set_shift_in(&dir(), entry, shift)
}

fn set_shift_in(d: &Path, entry: &Entry, shift: u16) -> std::io::Result<()> {
    let v = serde_json::to_value(entry).expect("entry serialises");
    change(d, &entry.id, |m| {
        for k in ["id", "name", "file", "repo"] {
            m.entry(k).or_insert_with(|| v[k].clone());
        }
        if shift == 0 {
            m.remove("shift");
        } else {
            m.insert("shift".into(), shift.into());
        }
    })
}

/// Write what the stack `id` claims; nothing when it claims that already.
pub fn set_claims(id: &str, claims: &Claims) -> std::io::Result<()> {
    set_claims_in(&dir(), id, claims)
}

fn set_claims_in(d: &Path, id: &str, claims: &Claims) -> std::io::Result<()> {
    if read_in(d, id).is_some_and(|e| &e.claims == claims) {
        return Ok(());
    }
    let v = serde_json::to_value(claims).expect("claims serialise");
    change(d, id, |m| {
        if claims.is_empty() {
            m.remove("claims");
        } else {
            m.insert("claims".into(), v);
        }
    })
}

/// The entry of the stack `id`, if it is registered.
pub fn read(id: &str) -> Option<Entry> {
    read_in(&dir(), id)
}

fn read_in(d: &Path, id: &str) -> Option<Entry> {
    serde_json::from_slice(&std::fs::read(path_of(d, id)).ok()?).ok()
}

pub fn list() -> Vec<Entry> {
    list_in(&dir())
}

fn list_in(d: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(d)
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

/// Another registered stack, as an allocation on this machine sees it.
#[derive(Debug, Clone)]
pub struct Other {
    pub entry: Entry,
    /// Its daemon runs.
    pub running: bool,
}

/// Every registered stack but `own_id` that still has its folder and its
/// stack file (one whose files are gone claims nothing), with whether its
/// daemon runs.
pub fn others(own_id: &str) -> Vec<Other> {
    list()
        .into_iter()
        .filter(|e| e.id != own_id && e.repo.is_dir() && e.file.is_file())
        .map(|entry| {
            let running = crate::read_info(&entry.repo.join(".devstack"))
                .is_some_and(|i| !i.stopping && devstack_proc::pid_alive(i.pid));
            Other { entry, running }
        })
        .collect()
}

/// Every port the stacks other than `own_id` claim.
pub fn foreign_claims(own_id: &str) -> BTreeSet<u16> {
    others(own_id)
        .iter()
        .flat_map(|o| o.entry.claims.ports())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            name: "shop".into(),
            file: PathBuf::from("/work/shop/devstack.toml"),
            repo: PathBuf::from("/work/shop"),
            ..Entry::default()
        }
    }

    #[test]
    fn an_entry_of_an_older_devstack_reads_as_unshifted_without_claims() {
        let old = r#"{"id":"abc","name":"shop","file":"/w/devstack.toml","repo":"/w"}"#;
        let e: Entry = serde_json::from_str(old).unwrap();
        assert_eq!(e.shift, 0);
        assert!(e.claims.is_empty());
        // Written back, it has no new fields an older devstack would see.
        let text = serde_json::to_string(&e).unwrap();
        assert!(
            !text.contains("shift") && !text.contains("claims"),
            "{text}"
        );
    }

    #[test]
    fn shift_and_claims_round_trip_and_survive_a_registration() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        register_in(d, &entry("abc")).unwrap();
        assert_eq!(read_in(d, "abc").unwrap(), entry("abc"));
        set_shift_in(d, &entry("abc"), 1000).unwrap();
        let claims = Claims {
            main: vec![6000, 6200],
            lanes: BTreeMap::from([("fix".to_string(), vec![6100, 6300])]),
        };
        set_claims_in(d, "abc", &claims).unwrap();
        // A field of a newer devstack, written by hand here.
        let path = path_of(d, "abc");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["future"] = "kept".into();
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        // The daemon registers again at its start: nothing else goes.
        let mut renamed = entry("abc");
        renamed.name = "shop-renamed".into();
        register_in(d, &renamed).unwrap();
        let e = read_in(d, "abc").unwrap();
        assert_eq!(e.name, "shop-renamed");
        assert_eq!(e.shift, 1000);
        assert_eq!(e.claims, claims);
        assert_eq!(e.claims.holder(6300), Some("fix"));
        assert_eq!(e.claims.holder(6000), Some("main"));
        assert_eq!(e.claims.ports().len(), 4);
        let raw = read_raw(d, "abc").unwrap();
        assert_eq!(raw["future"], "kept");
        // `--shift none` takes the field away again.
        set_shift_in(d, &entry("abc"), 0).unwrap();
        assert_eq!(read_in(d, "abc").unwrap().shift, 0);
        assert!(!read_raw(d, "abc").unwrap().contains_key("shift"));
        assert_eq!(list_in(d).len(), 1, "no temporary file is left");
    }

    #[test]
    fn a_shift_registers_a_stack_not_registered_yet() {
        let d = tempfile::tempdir().unwrap();
        set_shift_in(d.path(), &entry("def"), 2000).unwrap();
        let e = read_in(d.path(), "def").unwrap();
        assert_eq!((e.name.as_str(), e.shift), ("shop", 2000));
        assert!(change(d.path(), "../x", |_| {}).is_err());
    }
}
