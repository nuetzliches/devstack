//! Snapshot of a lane's source (ADR-0002), the prototype of spike (c) made
//! durable: tracked plus untracked non-ignored files are copied into the
//! lane's `src`; afterwards only files whose size or mtime changed. Copies get
//! the current time as mtime, so incremental build tools see them as newer
//! than their outputs (a restored file keeps an old mtime otherwise). The
//! manifest keeps a content hash per file for build ids.

use crate::{BuildError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
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

/// How long a sync waits after making new directories of one depth, before
/// it makes or writes anything inside them: a dev server's watcher takes a
/// new directory in when it sees it made (on the node, 10 ms were enough).
const NEW_DIR_SETTLE: Duration = Duration::from_millis(50);

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

    /// Remove the files under `under` that no sync put there (a build's
    /// outputs, intermediates, caches) and that were last written between
    /// `from` and `to`, a killed build's time (#523): what it left half
    /// written is written again by the next build, which an incremental
    /// tool would otherwise keep. Links are not followed. How many went.
    ///
    /// Another service's build that wrote under `under` at the same time
    /// loses those files too; its next build writes them again.
    pub fn discard_written(&self, under: &Path, from: SystemTime, to: SystemTime) -> usize {
        // A file's time is that of its last write; the kill takes a moment.
        let (from, to) = (
            from.checked_sub(Duration::from_secs(1)).unwrap_or(from),
            to + Duration::from_secs(2),
        );
        let mut removed = 0;
        let mut dirs = vec![under.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let Ok(ty) = e.file_type() else { continue };
                let path = e.path();
                let Ok(rel) = path.strip_prefix(&self.dir) else {
                    continue;
                };
                let rel = rel.to_string_lossy().replace('\\', "/");
                if EXCLUDED_PREFIXES
                    .iter()
                    .any(|x| format!("{rel}/").starts_with(x))
                {
                    continue;
                }
                if ty.is_dir() {
                    dirs.push(path);
                } else if ty.is_file()
                    && !self.manifest.files.contains_key(&rel)
                    && e.metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t >= from && t <= to)
                    && fs::remove_file(&path).is_ok()
                {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Tracked and untracked, non-ignored files of the source, with those of
    /// checked-out submodules (their own tracked and untracked files). Without
    /// a git repository (a node's mirror of a worktree, ADR-0029, holds exactly
    /// those already): every file but devstack's own state.
    pub fn list(&self) -> Result<Vec<String>> {
        if !in_git(&self.source) {
            return Ok(walk(&self.source));
        }
        let mut out = Vec::new();
        git_files(&self.source, "", &mut out)?;
        out.retain(|p| !EXCLUDED_PREFIXES.iter().any(|x| p.starts_with(x)));
        out.sort();
        Ok(out)
    }

    pub fn sync(&mut self) -> Result<SyncStats> {
        let t0 = Instant::now();
        let files = self.list()?;
        let listed: BTreeSet<&str> = files.iter().map(String::as_str).collect();
        let (mut copied, mut removed) = (0, 0);
        // Removals first: a case-only rename (`Foo.cs` -> `foo.cs`) is one
        // file on a case-insensitive file system, which a later removal of
        // the old name would take away again; and a file that became a
        // directory (`a` -> `a/b`) has to go before the directory is made.
        for gone in self
            .manifest
            .files
            .keys()
            .filter(|k| !listed.contains(k.as_str()))
        {
            let _ = fs::remove_file(self.dir.join(gone));
            removed += 1;
        }
        // A first sync has no watcher to wait for: nothing runs from the
        // copy yet.
        let settle = if self.manifest.files.is_empty() {
            Duration::ZERO
        } else {
            NEW_DIR_SETTLE
        };
        self.make_new_dirs(&files, settle)?;
        let mut next = BTreeMap::new();
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
            let dst = self.dir.join(rel);
            let unchanged = self
                .manifest
                .files
                .get(rel)
                .filter(|e| e.size == meta.len() && e.mtime == mtime && dst.is_file());
            let entry = match unchanged {
                Some(e) => e.clone(),
                None => {
                    self.make_room(rel)?;
                    let hash = copy_hashed(&src, &dst, now)?;
                    copied += 1;
                    Entry {
                        size: meta.len(),
                        mtime,
                        hash,
                    }
                }
            };
            next.insert(rel.clone(), entry);
        }
        // Listed, but no file any more (a directory now, or gone since).
        for stale in self
            .manifest
            .files
            .keys()
            .filter(|k| listed.contains(k.as_str()) && !next.contains_key(*k))
        {
            let _ = fs::remove_file(self.dir.join(stale));
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

    /// Clear the way for the copy of `rel`: a file where one of its parent
    /// directories goes, or a directory where the file goes, is what an
    /// earlier sync left of a path that changed its type.
    /// Make the directories `files` need that the copy lacks, one depth at a
    /// time, waiting `settle` after each (#507). A watcher on Linux that
    /// watches each directory and takes a new one in when it sees it made
    /// (`@parcel/watcher`, Angular's dev server) misses one made inside a new
    /// directory before it got there: made in one go (`create_dir_all`),
    /// `a/b/` left `b` unwatched, and edits to the files in it never reached
    /// the dev server. A file or link in the way is [`Self::make_room`]'s.
    fn make_new_dirs(&self, files: &[String], settle: Duration) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| BuildError::io(format!("create {}", self.dir.display()), e))?;
        let mut known: BTreeSet<PathBuf> = BTreeSet::new();
        let mut missing: BTreeMap<usize, BTreeSet<PathBuf>> = BTreeMap::new();
        for rel in files {
            let parts: Vec<&str> = rel.split('/').collect();
            let mut dir = self.dir.clone();
            for (depth, part) in parts[..parts.len().saturating_sub(1)].iter().enumerate() {
                dir.push(part);
                if known.contains(&dir) {
                    continue;
                }
                if missing.values().any(|m| m.contains(&dir)) {
                    continue;
                }
                match fs::symlink_metadata(&dir) {
                    Ok(m) if m.is_dir() => {
                        known.insert(dir.clone());
                    }
                    Ok(_) => break,
                    Err(_) => {
                        missing.entry(depth).or_default().insert(dir.clone());
                    }
                }
            }
        }
        for dirs in missing.values() {
            for d in dirs {
                match fs::create_dir(d) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(BuildError::io(format!("create {}", d.display()), e)),
                }
            }
            if !settle.is_zero() {
                std::thread::sleep(settle);
            }
        }
        Ok(())
    }

    fn make_room(&self, rel: &str) -> Result<()> {
        let dst = self.dir.join(rel);
        let mut dir = self.dir.clone();
        let parents: Vec<&str> = rel.split('/').collect();
        for part in &parents[..parents.len().saturating_sub(1)] {
            dir.push(part);
            if fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
                fs::remove_file(&dir)
                    .map_err(|e| BuildError::io(format!("remove {}", dir.display()), e))?;
            }
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| BuildError::io(format!("create {}", parent.display()), e))?;
        }
        if fs::symlink_metadata(&dst).is_ok_and(|m| m.is_dir()) {
            fs::remove_dir_all(&dst)
                .map_err(|e| BuildError::io(format!("remove {}", dst.display()), e))?;
        }
        Ok(())
    }
}

/// Copy `src` to `dst` in chunks, hashing on the way (a large binary is
/// never held in memory whole); the copy gets `stamp` as its mtime. Returns
/// the blake3 hash of the content, hex.
fn copy_hashed(src: &Path, dst: &Path, stamp: SystemTime) -> Result<String> {
    let mut from =
        fs::File::open(src).map_err(|e| BuildError::io(format!("read {}", src.display()), e))?;
    let mut to =
        fs::File::create(dst).map_err(|e| BuildError::io(format!("write {}", dst.display()), e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(BuildError::io(format!("read {}", src.display()), e)),
        };
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n])
            .map_err(|e| BuildError::io(format!("write {}", dst.display()), e))?;
    }
    to.set_modified(stamp)
        .map_err(|e| BuildError::io("stamp copy", e))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// The files `git ls-files` lists in `root/prefix`, as paths below `root`;
/// a submodule (listed as a directory) contributes its own files when it is
/// checked out, nothing when it is not (it is an empty directory then).
fn git_files(root: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let dir = root.join(prefix);
    let o = std::process::Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .current_dir(&dir)
        .output()
        .map_err(|e| BuildError::io("run git ls-files", e))?;
    if !o.status.success() {
        return Err(BuildError::Other(format!(
            "git ls-files failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    for p in String::from_utf8_lossy(&o.stdout).split('\0') {
        let p = p.trim_end_matches('/');
        if p.is_empty() {
            continue;
        }
        let rel = format!("{prefix}{p}");
        let path = root.join(&rel);
        if path.is_dir() {
            if path.join(".git").exists() {
                git_files(root, &format!("{rel}/"), out)?;
            }
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

/// Whether `dir` is in a git working tree: its root, or below it (a stack in
/// a subdirectory of a repository).
fn in_git(dir: &Path) -> bool {
    if dir.join(".git").exists() {
        return true;
    }
    std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"true"))
}

/// Every file below `root`, relative with `/`, without `.devstack/` and `.git/`.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel == ".git"
                || rel == ".devstack"
                || EXCLUDED_PREFIXES.iter().any(|x| rel.starts_with(x))
            {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push(rel),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tree_without_git_lists_every_file_but_devstacks_state() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/a")).unwrap();
        std::fs::create_dir_all(d.path().join(".devstack/lanes")).unwrap();
        std::fs::write(d.path().join("src/a/x.cs"), "x").unwrap();
        std::fs::write(d.path().join("devstack.toml"), "").unwrap();
        std::fs::write(d.path().join(".devstack/lanes/y"), "").unwrap();
        let state = tempfile::tempdir().unwrap();
        let snap = super::Snapshot::open(d.path(), state.path()).unwrap();
        assert_eq!(snap.list().unwrap(), vec!["devstack.toml", "src/a/x.cs"]);
    }

    /// A watcher like `@parcel/watcher` on Linux (Angular's dev server,
    /// #507): inotify on each directory, a new directory watched when its
    /// creation is seen in a watched one, never by reading it. Every
    /// directory the copy makes while a dev server runs has to be seen.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_watcher_that_follows_new_directories_sees_every_one_the_copy_makes() {
        use std::collections::HashMap;
        use std::ffi::{CString, OsStr};
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("app/lib")).unwrap();
        std::fs::write(src.path().join("app/lib/keep.ts"), "1").unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut snap = super::Snapshot::open(src.path(), state.path()).unwrap();
        snap.sync().unwrap();

        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0);
        let mask = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_MODIFY;
        let add = move |p: &std::path::Path| {
            let c = CString::new(p.as_os_str().as_bytes()).unwrap();
            unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) }
        };
        let mut dirs: HashMap<i32, PathBuf> = HashMap::new();
        for d in [
            snap.dir.clone(),
            snap.dir.join("app"),
            snap.dir.join("app/lib"),
        ] {
            dirs.insert(add(&d), d);
        }
        let seen: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (seen2, stop2) = (seen.clone(), stop.clone());
        let watcher = std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while !stop2.load(Ordering::SeqCst) {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                    continue;
                }
                // A dev server's watcher hands events on through its own
                // loop: on the node, `mkdir -p` (a process per level) was
                // already too fast for Angular's.
                std::thread::sleep(std::time::Duration::from_millis(5));
                let mut at = 0usize;
                while at < n as usize {
                    let ev = unsafe { &*(buf.as_ptr().add(at) as *const libc::inotify_event) };
                    let name_at = at + std::mem::size_of::<libc::inotify_event>();
                    let raw = &buf[name_at..name_at + ev.len as usize];
                    let name = OsStr::from_bytes(
                        &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())],
                    );
                    if ev.mask & libc::IN_ISDIR != 0
                        && ev.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0
                        && let Some(parent) = dirs.get(&ev.wd).cloned()
                    {
                        let p = parent.join(name);
                        dirs.insert(add(&p), p.clone());
                        seen2.lock().unwrap().push(p);
                    }
                    at = name_at + ev.len as usize;
                }
            }
            unsafe { libc::close(fd) };
        });

        for f in ["app/lib/new/inner/deep/a.ts", "app/fresh/b.ts", "top/c.ts"] {
            let p = src.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "2").unwrap();
        }
        snap.sync().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        stop.store(true, Ordering::SeqCst);
        watcher.join().unwrap();

        let seen = seen.lock().unwrap();
        for d in [
            "app/lib/new",
            "app/lib/new/inner",
            "app/lib/new/inner/deep",
            "app/fresh",
            "top",
        ] {
            assert!(
                seen.contains(&snap.dir.join(d)),
                "`{d}` was made where no watch saw it: {seen:?}"
            );
        }
    }
}
