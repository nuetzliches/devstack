//! The store of what `prepare` makes (ADR-0062): per machine and
//! repository, `<main tree>/.devstack/store/<service>/<key>/`, holding the
//! `outputs` at their paths in the repository. An entry is written once: a
//! temporary directory, its metadata written last, renamed to its key. A hit
//! is copied into a copy, never shared with one: tools write into what
//! `prepare` made.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// An entry's metadata, written last: a directory without it is no entry.
const META: &str = ".devstack-store.json";
/// Rewritten by each hit: when the entry was used last (its time), and how
/// long the copy out of it took (its content, milliseconds).
const USED: &str = ".devstack-store.used";
/// Files copied at once. Measured on Windows (24,500 files of a
/// `node_modules`): one at a time 13 s on NTFS, 16 or more at once 5 s
/// (ADR-0062, noted 2026-10-26). The time goes into the files, not the bytes.
const COPY_THREADS: usize = 32;
/// A temporary directory this old is left over from a daemon that ended
/// while it wrote an entry.
const TMP_STALE: Duration = Duration::from_secs(24 * 3600);

/// What an entry holds and how it came about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Meta {
    pub service: String,
    pub key: String,
    /// The directories it holds, relative to the repository.
    pub outputs: Vec<String>,
    /// Unix milliseconds.
    pub created: u128,
    /// How long the `prepare` that made it took.
    pub prepare_ms: u64,
    pub files: u64,
    pub bytes: u64,
}

/// What a copy did.
#[derive(Debug, Clone, Copy, Default)]
pub struct CopyStats {
    pub files: u64,
    pub bytes: u64,
    pub elapsed: Duration,
}

/// An entry the sweep found.
#[derive(Debug, Clone)]
pub struct Swept {
    pub service: String,
    pub key: String,
    pub bytes: u64,
    /// How long it was not used.
    pub idle: Duration,
    pub removed: bool,
    pub error: Option<String>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn entry(&self, service: &str, key: &str) -> PathBuf {
        self.root.join(service).join(key)
    }

    /// The entry of `key`, if it is complete.
    pub fn get(&self, service: &str, key: &str) -> Option<Meta> {
        read_meta(&self.entry(service, key))
    }

    /// How long the last copy out of the entry took.
    pub fn copy_ms(&self, service: &str, key: &str) -> Option<u64> {
        std::fs::read_to_string(self.entry(service, key).join(USED))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// The key, for this process: one `prepare` of a key at a time, the
    /// others wait and then find its entry. `None`: `cancel` said stop while
    /// it waited.
    pub fn lock(&self, service: &str, key: &str, cancel: &dyn Fn() -> bool) -> Option<KeyGuard> {
        static BUSY: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
        static FREED: Condvar = Condvar::new();
        let path = self.entry(service, key);
        let mut busy = BUSY.lock().unwrap();
        while busy.contains(&path) {
            if cancel() {
                return None;
            }
            busy = FREED
                .wait_timeout(busy, Duration::from_millis(200))
                .unwrap()
                .0;
        }
        busy.insert(path.clone());
        Some(KeyGuard {
            path,
            busy: &BUSY,
            freed: &FREED,
        })
    }

    /// Keep `outputs` of the copy at `from` (its repository root) as the
    /// entry of `key`. An entry that exists already stays as it is.
    pub fn put(
        &self,
        service: &str,
        key: &str,
        from: &Path,
        outputs: &[String],
        prepare: Duration,
    ) -> io::Result<Meta> {
        let entry = self.entry(service, key);
        if let Some(meta) = read_meta(&entry) {
            return Ok(meta);
        }
        let dir = self.root.join(service);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(
            ".tmp-{key}-{}-{}",
            std::process::id(),
            crate::state::now_ms()
        ));
        let written = (|| {
            // Links point to where the entry will be, not to `tmp`.
            let stats = copy_dirs(from, &tmp, &entry, outputs)?;
            let meta = Meta {
                service: service.to_string(),
                key: key.to_string(),
                outputs: outputs.to_vec(),
                created: crate::state::now_ms(),
                prepare_ms: prepare.as_millis() as u64,
                files: stats.files,
                bytes: stats.bytes,
            };
            std::fs::write(tmp.join(META), serde_json::to_vec_pretty(&meta)?)?;
            // What is left of an entry that never got its metadata.
            if entry.exists() {
                std::fs::remove_dir_all(&entry)?;
            }
            rename_dir(&tmp, &entry)?;
            Ok(meta)
        })();
        match written {
            Ok(meta) => Ok(meta),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                // Another process put the same key meanwhile.
                read_meta(&entry).ok_or(e)
            }
        }
    }

    /// Copy the entry's outputs into the copy at `to` (its repository
    /// root), replacing what is there under those paths.
    pub fn copy_into(&self, service: &str, key: &str, to: &Path) -> io::Result<CopyStats> {
        let entry = self.entry(service, key);
        let meta = read_meta(&entry)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such entry"))?;
        for rel in &meta.outputs {
            remove_path(&to.join(rel))?;
        }
        let stats = copy_dirs(&entry, to, to, &meta.outputs)?;
        let _ = std::fs::write(
            entry.join(USED),
            (stats.elapsed.as_millis() as u64).to_string(),
        );
        Ok(stats)
    }

    /// Entries not used for `older_than`, removed unless `dry_run`; and
    /// temporary directories a daemon that ended left behind.
    pub fn sweep(&self, older_than: Duration, dry_run: bool) -> Vec<Swept> {
        let now = SystemTime::now();
        let mut out = Vec::new();
        let Ok(services) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for svc in services.flatten() {
            let Ok(keys) = std::fs::read_dir(svc.path()) else {
                continue;
            };
            for k in keys.flatten() {
                let path = k.path();
                let name = k.file_name().to_string_lossy().into_owned();
                let age = |p: &Path| {
                    std::fs::metadata(p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                };
                if name.starts_with(".tmp-") {
                    if !dry_run && age(&path).is_some_and(|a| a >= TMP_STALE) {
                        let _ = std::fs::remove_dir_all(&path);
                    }
                    continue;
                }
                let Some(meta) = read_meta(&path) else {
                    continue;
                };
                let idle = age(&path.join(USED))
                    .or_else(|| age(&path.join(META)))
                    .unwrap_or_default();
                if idle < older_than {
                    continue;
                }
                let mut swept = Swept {
                    service: meta.service,
                    key: meta.key,
                    bytes: meta.bytes,
                    idle,
                    removed: false,
                    error: None,
                };
                if !dry_run {
                    // Out of the way first: a copy that starts meanwhile
                    // finds no entry rather than half of one.
                    let gone = svc.path().join(format!(".tmp-gone-{name}"));
                    match rename_dir(&path, &gone).and_then(|()| std::fs::remove_dir_all(&gone)) {
                        Ok(()) => swept.removed = true,
                        Err(e) => swept.error = Some(e.to_string()),
                    }
                }
                out.push(swept);
            }
        }
        out
    }
}

/// Holds a key of [`Store::lock`] until dropped.
pub struct KeyGuard {
    path: PathBuf,
    busy: &'static Mutex<BTreeSet<PathBuf>>,
    freed: &'static Condvar,
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        self.busy.lock().unwrap().remove(&self.path);
        self.freed.notify_all();
    }
}

fn read_meta(entry: &Path) -> Option<Meta> {
    let bytes = std::fs::read(entry.join(META)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A directory renamed; on Windows a scanner holding a file in it for a
/// moment makes the first tries fail.
fn rename_dir(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e)
                if cfg!(windows) && tries < 20 && e.kind() == io::ErrorKind::PermissionDenied =>
            {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            r => return r,
        }
    }
}

/// Whatever is at `p` removed: a link (a junction) without what it points
/// to, a directory with its content.
fn remove_path(p: &Path) -> io::Result<()> {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return Ok(());
    };
    if m.file_type().is_symlink() {
        // A junction or a directory link is a directory to Windows.
        #[cfg(windows)]
        if std::fs::remove_dir(p).is_ok() {
            return Ok(());
        }
        std::fs::remove_file(p)
    } else if m.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// The directories `rels` of `from` copied to the same paths under `to`,
/// files many at once. A link stays a link; one that points into `from`
/// points to the same place under `links_to` (npm's links to a workspace's
/// packages are absolute junctions on Windows): `to` itself, or where `to`
/// is renamed to.
pub fn copy_dirs(
    from: &Path,
    to: &Path,
    links_to: &Path,
    rels: &[String],
) -> io::Result<CopyStats> {
    let t0 = Instant::now();
    let mut files = Vec::new();
    for rel in rels {
        let src = from.join(rel);
        if !std::fs::symlink_metadata(&src).is_ok_and(|m| m.is_dir()) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("`{rel}` is no directory in {}", from.display()),
            ));
        }
        walk(from, links_to, &src, &to.join(rel), &mut files)?;
    }
    let next = AtomicUsize::new(0);
    let bytes = AtomicU64::new(0);
    let failed: Mutex<Option<io::Error>> = Mutex::new(None);
    let threads = COPY_THREADS.min(files.len()).max(1);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() || failed.lock().unwrap().is_some() {
                        break;
                    }
                    let (src, dst) = &files[i];
                    match std::fs::copy(src, dst) {
                        Ok(n) => {
                            bytes.fetch_add(n, Ordering::Relaxed);
                        }
                        Err(e) => {
                            *failed.lock().unwrap() =
                                Some(io::Error::new(e.kind(), format!("{}: {e}", src.display())));
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }
    Ok(CopyStats {
        files: files.len() as u64,
        bytes: bytes.into_inner(),
        elapsed: t0.elapsed(),
    })
}

/// `src` made at `dst`: its directories and links at once, its files into
/// `files` for [`copy_dirs`] to copy. A link into `from` goes to `to`.
fn walk(
    from: &Path,
    to: &Path,
    src: &Path,
    dst: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let ft = e.file_type()?;
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if ft.is_symlink() {
            copy_link(from, to, &s, &d)?;
        } else if ft.is_dir() {
            walk(from, to, &s, &d, files)?;
        } else {
            files.push((s, d));
        }
    }
    Ok(())
}

/// The link `src` made again at `dst`, its target moved from `from` to `to`
/// when it pointed into `from`.
fn copy_link(from: &Path, to: &Path, src: &Path, dst: &Path) -> io::Result<()> {
    let target = std::fs::read_link(src)?;
    let target = match plain(&target).strip_prefix(plain(from)) {
        Ok(rest) if target.is_absolute() => to.join(rest),
        _ => target,
    };
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        let abs = if target.is_absolute() {
            target.clone()
        } else {
            dst.parent().unwrap_or(to).join(&target)
        };
        if std::fs::symlink_metadata(src)?.file_type().is_symlink_dir() {
            // A junction needs no privilege, a directory symbolic link does.
            junction::create(&abs, dst)
        } else {
            std::os::windows::fs::symlink_file(&target, dst)
                .or_else(|_| std::fs::copy(&abs, dst).map(|_| ()))
        }
    }
}

/// `\\?\C:\x` as `C:\x`, so that a link's target and a path compare.
fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix(r"\??\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) {
        let nm = root.join("web/node_modules");
        std::fs::create_dir_all(nm.join("pkg/lib")).unwrap();
        std::fs::write(nm.join("pkg/index.js"), "module.exports = 1").unwrap();
        std::fs::write(nm.join("pkg/lib/a.js"), "a").unwrap();
        std::fs::create_dir_all(root.join("packages/shared")).unwrap();
        std::fs::write(root.join("packages/shared/index.js"), "shared").unwrap();
        // A workspace package, linked as npm links it: absolute on Windows
        // (a junction), relative elsewhere.
        #[cfg(windows)]
        junction::create(root.join("packages/shared"), nm.join("shared")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../../packages/shared", nm.join("shared")).unwrap();
    }

    #[test]
    fn a_put_entry_is_copied_into_another_copy_links_and_all() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        tree(&a);
        std::fs::create_dir_all(b.join("packages/shared")).unwrap();
        std::fs::write(b.join("packages/shared/index.js"), "b's shared").unwrap();
        let store = Store::new(d.path().join("store"));
        let outputs = vec!["web/node_modules".to_string()];
        assert!(store.get("web", "k1").is_none());
        let meta = store
            .put("web", "k1", &a, &outputs, Duration::from_secs(30))
            .unwrap();
        assert_eq!(meta.files, 2);
        assert_eq!(meta.prepare_ms, 30_000);
        assert_eq!(store.get("web", "k1"), Some(meta));
        // What was there before goes.
        std::fs::create_dir_all(b.join("web/node_modules/old")).unwrap();
        let stats = store.copy_into("web", "k1", &b).unwrap();
        assert_eq!(stats.files, 2);
        let nm = b.join("web/node_modules");
        assert!(!nm.join("old").exists());
        assert_eq!(
            std::fs::read_to_string(nm.join("pkg/lib/a.js")).unwrap(),
            "a"
        );
        // The link points into the copy it is in, not the one it came from.
        assert!(
            std::fs::symlink_metadata(nm.join("shared"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(nm.join("shared/index.js")).unwrap(),
            "b's shared"
        );
        assert!(store.copy_ms("web", "k1").is_some());
        // A copy writes into its own files, never into the entry's.
        std::fs::write(nm.join("pkg/index.js"), "changed").unwrap();
        let c = d.path().join("c");
        store.copy_into("web", "k1", &c).unwrap();
        assert_eq!(
            std::fs::read_to_string(c.join("web/node_modules/pkg/index.js")).unwrap(),
            "module.exports = 1"
        );
    }

    #[test]
    fn a_second_put_keeps_the_first_entry_and_a_missing_output_is_none() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        tree(&a);
        let store = Store::new(d.path().join("store"));
        let outputs = vec!["web/node_modules".to_string()];
        store
            .put("web", "k", &a, &outputs, Duration::from_secs(1))
            .unwrap();
        std::fs::write(a.join("web/node_modules/pkg/index.js"), "other").unwrap();
        let again = store
            .put("web", "k", &a, &outputs, Duration::from_secs(9))
            .unwrap();
        assert_eq!(again.prepare_ms, 1000, "the first entry stays");
        let err = store
            .put("web", "k2", &a, &["web/dist".to_string()], Duration::ZERO)
            .unwrap_err();
        assert!(err.to_string().contains("web/dist"), "{err}");
        assert!(store.get("web", "k2").is_none());
        let left: Vec<_> = std::fs::read_dir(d.path().join("store/web"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 1, "no temporary directory left: {left:?}");
    }

    #[test]
    fn the_sweep_takes_entries_unused_for_long_enough() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        tree(&a);
        let store = Store::new(d.path().join("store"));
        let outputs = vec!["web/node_modules".to_string()];
        store
            .put("web", "k", &a, &outputs, Duration::from_secs(1))
            .unwrap();
        assert!(store.sweep(Duration::from_secs(3600), false).is_empty());
        let found = store.sweep(Duration::ZERO, true);
        assert_eq!(found.len(), 1);
        assert!(!found[0].removed);
        assert!(store.get("web", "k").is_some(), "a dry run keeps it");
        let swept = store.sweep(Duration::ZERO, false);
        assert!(swept[0].removed, "{swept:?}");
        assert!(store.get("web", "k").is_none());
    }

    #[test]
    fn a_key_is_prepared_by_one_at_a_time() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::new(d.path());
        let first = store.lock("web", "k", &|| false).unwrap();
        assert!(store.lock("web", "other", &|| false).is_some());
        assert!(
            store.lock("web", "k", &|| true).is_none(),
            "a stop ends the wait"
        );
        let waiter = std::thread::spawn(move || {
            let store = Store::new(d.path());
            let t0 = Instant::now();
            drop(store.lock("web", "k", &|| false).unwrap());
            (t0.elapsed(), d)
        });
        std::thread::sleep(Duration::from_millis(300));
        drop(first);
        let (waited, _d) = waiter.join().unwrap();
        assert!(waited >= Duration::from_millis(250), "{waited:?}");
    }
}
