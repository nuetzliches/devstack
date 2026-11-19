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

/// Why an entry's file did not read.
enum Unread {
    /// The file is there, and still does not read after the retries.
    Io(std::io::Error),
    /// It reads, and is no JSON of the kind asked for.
    Parse(serde_json::Error),
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unread::Io(e) => e.fmt(f),
            Unread::Parse(e) => e.fmt(f),
        }
    }
}

/// What the file `path` holds, parsed; `Ok(None)` once it is gone. A read
/// that meets the file while its daemon replaces it (on Windows the rename
/// over it can fail a reader for a moment), or finds it cut off, is tried
/// again for up to 200 ms: an entry skipped reads as a stack that claims
/// nothing, and an entry written back from nothing loses its id (#535,
/// #539). Content that reads and does not parse is given up at once.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, Unread> {
    let mut tries = 0;
    loop {
        tries += 1;
        let last = tries == 10;
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(v) => return Ok(Some(v)),
                Err(e) if e.is_eof() && !last => {}
                Err(e) => return Err(Unread::Parse(e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if last => return Err(Unread::Io(e)),
            Err(_) => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Said in the log once per file: an entry that does not read.
fn unreadable(path: &Path, error: &Unread) {
    static SAID: std::sync::Mutex<BTreeSet<PathBuf>> = std::sync::Mutex::new(BTreeSet::new());
    if SAID.lock().unwrap().insert(path.to_path_buf()) {
        crate::diag::warn(
            "registry_entry_unreadable",
            serde_json::json!({ "file": path, "error": error.to_string() }),
        );
    }
}

/// The entry file as it is, as JSON: what a write keeps; `Ok(None)` when
/// there is none. An error when it is there and does not read as a JSON
/// object, said in the log once (`InvalidData` when it reads and does not
/// parse): a write then must not replace it with only what it sets (#570).
fn read_raw(
    d: &Path,
    id: &str,
) -> std::io::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let path = path_of(d, id);
    let e = match read_json::<serde_json::Value>(&path) {
        Ok(Some(serde_json::Value::Object(m))) => return Ok(Some(m)),
        Ok(None) => return Ok(None),
        Ok(Some(_)) => Unread::Parse(serde::de::Error::custom("not a JSON object")),
        Err(e) => e,
    };
    unreadable(&path, &e);
    Err(match e {
        Unread::Io(e) => e,
        Unread::Parse(e) => std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })
}

/// Change the entry `id` in `d` by `f`, keeping every field `f` does not
/// touch. Written to a file of this process first, then renamed over the
/// entry: a reader never sees half of it, and two writers never share the
/// temporary file. An entry that does not parse is an error, unless
/// `replace_unparsed`: only a write that names the whole stack may start it
/// anew (#570).
fn change(
    d: &Path,
    id: &str,
    replace_unparsed: bool,
    f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> std::io::Result<()> {
    if !id.chars().all(|c| c.is_ascii_hexdigit()) || id.is_empty() {
        return Err(std::io::Error::other(format!("not a stack id: {id}")));
    }
    std::fs::create_dir_all(d)?;
    let mut m = match read_raw(d, id) {
        Ok(m) => m.unwrap_or_default(),
        Err(e) if replace_unparsed && e.kind() == std::io::ErrorKind::InvalidData => {
            serde_json::Map::new()
        }
        Err(e) => return Err(e),
    };
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
/// and claims, and whatever else the entry holds, stay; an entry that does
/// not parse is written anew, which its daemon's start does.
pub fn register(entry: &Entry) -> std::io::Result<()> {
    register_in(&dir(), entry)
}

fn register_in(d: &Path, entry: &Entry) -> std::io::Result<()> {
    let v = serde_json::to_value(entry).expect("entry serialises");
    change(d, &entry.id, true, |m| {
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
    change(d, &entry.id, false, |m| {
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

/// Write what the stack `entry` claims; nothing when it claims that already.
/// Its daemon holds the whole entry, its shift too: one that does not parse
/// is registered anew from it first, rather than left unseen (#570).
pub fn set_claims(entry: &Entry, claims: &Claims) -> std::io::Result<()> {
    set_claims_of_in(&dir(), entry, claims)
}

fn set_claims_of_in(d: &Path, entry: &Entry, claims: &Claims) -> std::io::Result<()> {
    match set_claims_in(d, &entry.id, claims) {
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            register_in(d, entry)?;
            if entry.shift > 0 {
                set_shift_in(d, entry, entry.shift)?;
            }
            set_claims_in(d, &entry.id, claims)
        }
        r => r,
    }
}

fn set_claims_in(d: &Path, id: &str, claims: &Claims) -> std::io::Result<()> {
    if read_in(d, id).is_some_and(|e| &e.claims == claims) {
        return Ok(());
    }
    let v = serde_json::to_value(claims).expect("claims serialise");
    change(d, id, false, |m| {
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
    read_entry(&path_of(d, id))
}

pub fn list() -> Vec<Entry> {
    list_in(&dir())
}

/// The entry in the file `path` ([`read_json`]); `None` once it is gone, or
/// when it does not read, which is said in the log once per file.
fn read_entry(path: &Path) -> Option<Entry> {
    read_json(path).unwrap_or_else(|e| {
        unreadable(path, &e);
        None
    })
}

fn list_in(d: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_entry(&e.path()))
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
        let raw = read_raw(d, "abc").unwrap().unwrap();
        assert_eq!(raw["future"], "kept");
        // `--shift none` takes the field away again.
        set_shift_in(d, &entry("abc"), 0).unwrap();
        assert_eq!(read_in(d, "abc").unwrap().shift, 0);
        assert!(!read_raw(d, "abc").unwrap().unwrap().contains_key("shift"));
        assert_eq!(list_in(d).len(), 1, "no temporary file is left");
    }

    #[test]
    fn a_shift_registers_a_stack_not_registered_yet() {
        let d = tempfile::tempdir().unwrap();
        set_shift_in(d.path(), &entry("def"), 2000).unwrap();
        let e = read_in(d.path(), "def").unwrap();
        assert_eq!((e.name.as_str(), e.shift), ("shop", 2000));
        assert!(change(d.path(), "../x", false, |_| {}).is_err());
    }

    /// #539: an entry met while its daemon replaces it is read again, not
    /// taken for a stack that claims nothing (its lanes' ports then looked
    /// free to another stack's numbering).
    #[test]
    fn an_entry_met_while_it_is_replaced_is_read_again() {
        let d = tempfile::tempdir().unwrap();
        let path = path_of(d.path(), "abc");
        // Half of it now (what a reader sees of a file not yet complete, or
        // not at all while it is renamed over), all of it a moment later.
        std::fs::write(&path, "{\"id\":\"abc\",").unwrap();
        let mut whole = entry("abc");
        whole.claims.main = vec![4600];
        let full = serde_json::to_vec(&whole).unwrap();
        let later = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            std::fs::write(later, full).unwrap();
        });
        let read = read_entry(&path);
        writer.join().unwrap();
        assert_eq!(read.map(|e| e.claims.main), Some(vec![4600]));
        // Gone is gone at once; what never parses is given up.
        assert!(read_entry(&path_of(d.path(), "nope")).is_none());
        std::fs::write(&path, "not json").unwrap();
        assert!(read_entry(&path).is_none());
    }

    /// A write that meets its entry while it is replaced reads it again,
    /// rather than writing back only what it sets: an entry without its id
    /// and name drops out of `list` with its claims (code review after
    /// 0.47.0, finding 1).
    #[test]
    fn a_write_keeps_the_entry_it_met_while_it_was_replaced() {
        let d = tempfile::tempdir().unwrap();
        let path = path_of(d.path(), "abc");
        std::fs::write(&path, "{\"id\":\"abc\",").unwrap();
        let full = serde_json::to_vec(&entry("abc")).unwrap();
        let later = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            std::fs::write(later, full).unwrap();
        });
        let claims = Claims {
            main: vec![4600],
            ..Claims::default()
        };
        set_claims_in(d.path(), "abc", &claims).unwrap();
        writer.join().unwrap();
        let e = read_in(d.path(), "abc").expect("still an entry");
        assert_eq!((e.name.as_str(), e.claims), ("shop", claims));
    }

    /// #570: an entry that does not read as one, after its retries, is not
    /// written over by a write that sets only some fields: it would lose its
    /// id and name, drop out of `list`, and its claims with it. A
    /// registration names the whole stack and may replace it.
    #[test]
    fn a_write_of_some_fields_keeps_an_entry_that_does_not_read() {
        let d = tempfile::tempdir().unwrap();
        let path = path_of(d.path(), "abc");
        let claims = Claims {
            main: vec![4600],
            ..Claims::default()
        };
        for broken in ["{\"id\":\"abc\",", "[]", "not json"] {
            std::fs::write(&path, broken).unwrap();
            assert!(set_claims_in(d.path(), "abc", &claims).is_err(), "{broken}");
            assert!(
                set_shift_in(d.path(), &entry("abc"), 1000).is_err(),
                "{broken}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
            register_in(d.path(), &entry("abc")).unwrap();
            assert_eq!(read_in(d.path(), "abc"), Some(entry("abc")), "{broken}");
        }
    }

    /// A daemon whose entry stopped parsing while it runs writes it anew from
    /// the whole entry it holds, with its shift, and then its claims: else its
    /// claims stay unseen by other stacks until it starts again (code review
    /// before 0.51.0, finding 5).
    #[test]
    fn the_daemons_claims_write_an_entry_that_does_not_parse_anew() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(path_of(d.path(), "abc"), "[]").unwrap();
        let mut own = entry("abc");
        own.shift = 1000;
        let claims = Claims {
            main: vec![5600],
            ..Claims::default()
        };
        set_claims_of_in(d.path(), &own, &claims).unwrap();
        let e = read_in(d.path(), "abc").expect("an entry again");
        assert_eq!((e.name.as_str(), e.shift), ("shop", 1000));
        assert_eq!(e.claims, claims);
    }

    /// What never parses is not waited for: every `list` would pay for it
    /// (code review after 0.47.0, finding 2).
    #[test]
    fn an_entry_that_is_no_entry_is_given_up_at_once() {
        let d = tempfile::tempdir().unwrap();
        let path = path_of(d.path(), "abc");
        std::fs::write(&path, "[]").unwrap();
        let t0 = std::time::Instant::now();
        assert!(read_entry(&path).is_none());
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(100),
            "{:?}",
            t0.elapsed()
        );
    }
}
