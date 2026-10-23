//! The file watcher (plan §3.2): changes in the working tree, matched against
//! what each service declares.
//!
//! - built services with `watch`: a candidate (ADR-0003, `Supervisor::changed`);
//! - dev servers with `restart_on`: a restart in place;
//! - Compose services with `restart_on`: a container restart;
//! - the stack file itself: the status says it changed since the daemon started
//!   (the daemon does not reload it);
//! - in a lane, anything: the copy its dev servers and tools run from follows
//!   (ADR-0024).
//!
//! Generated directories never count (`bin`, `obj`, `node_modules`, `target`,
//! `.git`, `.devstack`, and the caches of frontend tools and Python:
//! `.angular`, `.vite`, `.next`, `__pycache__`, ...): a build in the
//! repository, or a dev server as it starts, would otherwise see its own
//! output as a change. They are not even looked at: an event is judged on its
//! path alone, before the file system is asked anything about it. A package
//! manager creates thousands of directories in `node_modules` in a burst, and
//! a watcher that opens each one as it appears races the tool: on Windows
//! `pnpm install` (libuv) makes a junction by creating the directory and then
//! opening it with no sharing, and a listing of the directory in between makes
//! that open fail with `EBUSY`. For the same reason Windows lists no new
//! directory at all (`LIST_NEW_DIRS`).
//!
//! What git ignores makes no candidate either, nor does it update a lane's
//! copy ([`gitignore`]): a build runs in a snapshot that holds no ignored
//! file, so such a change cannot change a build. A dev server's or a
//! container's `restart_on` and the stack file are judged by the list above
//! alone: those run from (or are) the working tree itself, where an ignored
//! file (`.env`, `appsettings.Development.json`) is read, and its change is
//! worth a restart when `restart_on` names it.

use crate::supervisor::Supervisor;
use devstack_config::model::Kind;
use globset::GlobSet;
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

mod gitignore;
use gitignore::Ignores;

/// Keeps the OS watch alive; dropping it stops watching.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
}

fn set(patterns: &[String]) -> Option<GlobSet> {
    devstack_build::watch_set(patterns).ok().flatten()
}

/// Does `restart_on` (compiled to `set`) cover `rel`: does a pattern match it,
/// or a directory it is in? `restart_on = ["src/api"]` means the folder and
/// what it holds. As a glob, `src/api` matches the folder's own path only,
/// and the OS reports a change to a file by the file's path. Linux never
/// reports the folder for it; Windows reports the folder itself (its time
/// stamps changed) for some creations and deletions, depending on the tool
/// that made them, and never for a saved file. So the restart came with
/// whichever later event happened to name the folder: a second, minutes or
/// the next change later (#221).
///
/// For `restart_on` only: a restart reads the working tree, so a folder means
/// what is in it. `watch` also selects a build's input files
/// (`devstack_build`), file by file; the watcher matches it the same way.
fn covers(set: &GlobSet, rel: &str) -> bool {
    set.is_match(rel) || rel.match_indices('/').any(|(i, _)| set.is_match(&rel[..i]))
}

/// Is `name`, one component of a path, a directory of git, of devstack or of
/// a build's output?
fn generated(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".devstack" | "node_modules" | "bin" | "obj" | "target"
    )
}

/// Is `name` the cache directory of a frontend tool, or of Python? Usually
/// ignored by git as well; listed for a repository whose `.gitignore` does not
/// say so, and so that the watcher never opens them (module doc). A dev
/// server writes into them as it starts (Vite's prebundled dependencies in
/// `.angular/cache/<version>/<project>/vite/deps`, a `package.json` among
/// them; Python's compiled modules in `__pycache__` next to each changed
/// source, which a `restart_on = ["src/api"]` covers: counted, the restart
/// for a change would restart once more for what the new process compiled).
fn cache(name: &str) -> bool {
    matches!(
        name,
        ".angular"
            | ".vite"
            | ".next"
            | ".nuxt"
            | ".turbo"
            | ".cache"
            | ".svelte-kit"
            | ".parcel-cache"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
    )
}

/// Is `name` the file name of a write in progress, renamed onto the real
/// file a moment later? Atomic writes of editors and agents' tools
/// (`db.rs.tmp.33128.2dcc67a056d2`: name, `.tmp.`, process id, random hex),
/// GNU `sed -i` (`sedGr1VFQ`), vim's probe file `4913` and its swap files,
/// Emacs' lock and backup files. The real file's change follows; these
/// would only put names in a candidate's paths that never were the user's.
fn transient(name: &str) -> bool {
    let mut parts = name.rsplitn(3, '.');
    let write_atomic = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(hex), Some(pid), Some(rest))
            if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
                && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
                && rest.ends_with(".tmp")
    );
    // Six random letters and digits; a real name of that shape (`sedimentd`)
    // is lower case throughout.
    let sed = name.len() == 9
        && name.starts_with("sed")
        && name[3..].bytes().all(|b| b.is_ascii_alphanumeric())
        && name[3..]
            .bytes()
            .any(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    write_atomic
        || sed
        || name == "4913"
        || name.ends_with('~')
        || name.starts_with(".#")
        || (name.starts_with('.') && (name.ends_with(".swp") || name.ends_with(".swx")))
}

/// Is `rel` (repository-relative, `/`-separated) in a directory that never
/// counts (a generated one or a tool's cache), or a write in progress?
pub fn ignored(rel: &str) -> bool {
    rel.split('/').any(|n| generated(n) || cache(n))
        || rel.rsplit('/').next().is_some_and(transient)
}

/// Is `rel` in a generated directory (not a cache)? What tells a remote
/// lane's sync (ADR-0029) that a change needs no sync: a cache is a file of
/// the worktree like any other there, sent when git does not ignore it.
pub fn in_generated(rel: &str) -> bool {
    rel.split('/').any(generated)
}

/// `path` as a repository-relative, `/`-separated string, or `None` when it
/// does not count: outside the repository, the repository itself, or in a
/// generated directory. Text only, no file system call; whatever needs one
/// asks this first.
fn counted(repo: &Path, path: &Path) -> Option<String> {
    let rel = path
        .strip_prefix(repo)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    (!rel.is_empty() && !ignored(&rel)).then_some(rel)
}

/// `path` under `repo` when the OS reported it under `real`, the repository's
/// real path. FSEvents (macOS) reports real paths: `/private/tmp/...` for a
/// repository opened as `/tmp/...`, the target of a symlinked project folder,
/// the case the disk has. Elsewhere `real` never prefixes an event's path
/// that `repo` does not, and nothing changes.
fn as_given(repo: &Path, real: &Path, path: PathBuf) -> PathBuf {
    if path.starts_with(repo) {
        return path;
    }
    match path.strip_prefix(real) {
        Ok(rel) => repo.join(rel),
        Err(_) => path,
    }
}

/// Does a change to `path` change the ignore rules? Asked of the event's own
/// paths: `.git/info/exclude` is in a directory that never counts.
fn rule_file(repo: &Path, path: &Path) -> bool {
    path.strip_prefix(repo)
        .is_ok_and(|r| gitignore::is_rule_file(&r.to_string_lossy().replace('\\', "/")))
}

/// The two things the watcher asks the file system. A link to a directory is
/// a leaf, never entered: its target is somewhere else (and watched there when
/// it is in the repository), and a link to an ancestor would never end.
trait Fs {
    /// Is `path` a directory itself (not a link to one)?
    fn is_dir(&self, path: &Path) -> bool;
    /// What `dir` holds now, each entry with whether it is a directory
    /// itself; nothing when it cannot be read. The listing is complete before
    /// this returns: nothing keeps the directory open while the caller works.
    fn entries(&self, dir: &Path) -> Vec<(PathBuf, bool)>;
}

struct Disk;

impl Fs for Disk {
    fn is_dir(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
    }

    fn entries(&self, dir: &Path) -> Vec<(PathBuf, bool)> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| (e.path(), e.file_type().is_ok_and(|t| t.is_dir())))
            .collect()
    }
}

/// Whether a directory that has just been created is listed for the files it
/// already holds. inotify and kqueue need it: they watch the directory only
/// from now on, so files created in it right away are missed. FSEvents and
/// ReadDirectoryChangesW report everything created in a new directory
/// themselves, and on Windows a listing is an open with read access that can
/// make the creating tool's own exclusive open fail (module doc). What Windows
/// loses: a directory that arrives already populated, moved in from outside
/// the tree or from one folder of it to another, is reported as itself,
/// without its files. A rename within one folder, and any move on Linux
/// (inotify reports a rename), never listed them either.
const LIST_NEW_DIRS: bool = !cfg!(windows);

/// The paths of an event that count, listing a new directory as the platform
/// needs ([`LIST_NEW_DIRS`]).
fn rels(repo: &Path, ev: notify::Event) -> Vec<String> {
    event_paths(repo, ev, &Disk, LIST_NEW_DIRS)
}

/// The paths an event is about that count, repository-relative. With
/// `list_new_dirs`, a directory that was just created is reported with the
/// files it already holds; generated directories are neither listed nor
/// entered.
fn event_paths(repo: &Path, ev: notify::Event, fs: &dyn Fs, list_new_dirs: bool) -> Vec<String> {
    if matches!(ev.kind, EventKind::Access(_)) {
        return Vec::new();
    }
    let list = list_new_dirs && matches!(ev.kind, EventKind::Create(_));
    let mut out = Vec::new();
    for path in ev.paths {
        let Some(rel) = counted(repo, &path) else {
            continue;
        };
        out.push(rel);
        if list && fs.is_dir(&path) {
            let mut dirs = vec![path];
            while let Some(dir) = dirs.pop() {
                for (p, is_dir) in fs.entries(&dir) {
                    let Some(rel) = counted(repo, &p) else {
                        continue;
                    };
                    if is_dir {
                        dirs.push(p);
                    } else {
                        out.push(rel);
                    }
                }
            }
        }
    }
    out
}

/// Start watching the repository (only its top directory, for the stack file,
/// when nothing in the stack file asks for more); `None` when the OS watch
/// could not be set up, which the status and the log then say. When the OS
/// drops events (an overflow, a checkout of thousands of files), every built
/// service gets a candidate, as if all of its files had changed.
pub fn start(sup: Arc<Supervisor>) -> Option<Watcher> {
    match watch(sup.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            sup.set_watcher_error(Some(format!(
                "the file watcher could not be set up on {}: {e}; edits make no candidates (restart a service to pick them up)",
                sup.repo.display()
            )));
            None
        }
    }
}

fn watch(sup: Arc<Supervisor>) -> Result<Watcher, String> {
    let platform = devstack_config::model::Platform::current();
    let in_lane = !sup.lane.is_empty();
    let runs = |s: &devstack_config::model::Service| {
        (s.platforms.is_empty() || s.platforms.contains(&platform)) && (!in_lane || s.lanes)
    };
    let built: Vec<(String, GlobSet)> = sup
        .stack
        .services
        .iter()
        .filter(|(_, s)| s.build.is_some() && runs(s))
        .filter_map(|(n, s)| set(&s.watch).map(|g| (n.clone(), g)))
        .collect();
    let devservers: Vec<(String, GlobSet)> = sup
        .stack
        .services
        .iter()
        .filter(|(_, s)| s.kind == Kind::Devserver && runs(s))
        .filter_map(|(n, s)| set(&s.restart_on).map(|g| (n.clone(), g)))
        .collect();
    // A lane restarts its own containers, never the `shared` ones of the
    // main tree (ADR-0030).
    let containers: Vec<(String, String, GlobSet)> = sup
        .stack
        .compose
        .iter()
        .flat_map(|(g, c)| {
            c.restart_on
                .iter()
                .filter(move |(s, _)| !in_lane || !c.shared.contains(s))
                .filter_map(move |(s, p)| set(p).map(|set| (g.clone(), s.clone(), set)))
        })
        .collect();
    let lane = !sup.lane.is_empty();
    let only_stack_file =
        built.is_empty() && devservers.is_empty() && containers.is_empty() && !lane;
    let stack_rel = sup
        .stack_file()
        .strip_prefix(&sup.repo)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    let repo = sup.repo.clone();
    let real = std::fs::canonicalize(&repo).unwrap_or_else(|_| repo.clone());
    let me = sup.clone();
    let lane_copy = lane;
    let pending = Arc::new(Pending::default());
    // Read once here and again when a rule file changes, never per event.
    let rules = Arc::new(RwLock::new(Arc::new(if only_stack_file {
        Ignores::none()
    } else {
        Ignores::load(&repo)
    })));
    let rescan = {
        let (me, built) = (
            sup.clone(),
            built.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        );
        move |why: String| {
            me.set_watcher_error(Some(format!(
                "the file watcher lost events ({why}); every built service was rebuilt as a candidate"
            )));
            if lane {
                me.copy_changed();
            }
            for n in &built {
                me.changed(n, crate::supervisor::RESCAN);
            }
        }
    };
    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let mut ev = match res {
            Ok(ev) if ev.need_rescan() => return rescan("the OS asked for a rescan".into()),
            Ok(ev) => ev,
            Err(e) => return rescan(e.to_string()),
        };
        ev.paths = std::mem::take(&mut ev.paths)
            .into_iter()
            .map(|p| as_given(&repo, &real, p))
            .collect();
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        me.watcher_ok();
        if !only_stack_file && ev.paths.iter().any(|p| rule_file(&repo, p)) {
            let (rules, repo) = (rules.clone(), repo.clone());
            me.debounce("gitignore".into(), move |_| {
                let fresh = Arc::new(Ignores::load(&repo));
                *rules.write().unwrap() = fresh;
            });
        }
        let ignores = rules.read().unwrap().clone();
        for rel in rels(&repo, ev) {
            // An ignored file is not in a snapshot: no input of a build, no
            // part of a lane's copy (module doc).
            let in_snapshot = !ignores.ignored(&rel);
            if lane_copy && in_snapshot {
                me.copy_changed();
            }
            if stack_rel.as_deref() == Some(rel.as_str()) {
                me.stack_file_touched();
                continue;
            }
            if in_snapshot {
                // A build file (`Directory.Build.targets`, a lockfile) may be
                // an input of any build; the build id decides (#13).
                let build_file = devstack_build::is_build_file(&rel);
                for (n, g) in &built {
                    if g.is_match(&rel) || build_file {
                        me.changed(n, &rel);
                    }
                }
            }
            for (n, g) in &devservers {
                if covers(g, &rel) {
                    let key = format!("devserver:{n}");
                    pending.add(&key, &rel);
                    let (n, p) = (n.clone(), pending.clone());
                    me.debounce(key.clone(), move |s| restart_devserver(s, key, n, p));
                }
            }
            for (group, service, g) in &containers {
                if covers(g, &rel) {
                    let key = format!("compose:{group}.{service}");
                    pending.add(&key, &rel);
                    let (group, service, p) = (group.clone(), service.clone(), pending.clone());
                    me.debounce(key.clone(), move |s| {
                        let Some(seen) = p.take(&key) else {
                            return;
                        };
                        seen.log(
                            "restart_on",
                            serde_json::json!({
                                "lane": s.lane,
                                "group": group,
                                "service": service,
                            }),
                        );
                        s.restart_container(&group, &service);
                    });
                }
            }
        }
    })
    .map_err(|e| e.to_string())?;
    let mode = if only_stack_file {
        RecursiveMode::NonRecursive
    } else {
        RecursiveMode::Recursive
    };
    inner.watch(&sup.repo, mode).map_err(|e| e.to_string())?;
    sup.set_watcher_error(None);
    Ok(Watcher { _inner: inner })
}

/// The changes each `restart_on` restart waits for, by its debounce key: what
/// the daemon log names when the restart fires, so that a restart that came
/// late can be told from a change that was seen late (#221).
#[derive(Default)]
struct Pending(Mutex<BTreeMap<String, Seen>>);

/// The changes under one service's `restart_on` since its last restart.
struct Seen {
    /// When the first of them was seen: the wait, and the time as the log's
    /// `at` writes it.
    first: Instant,
    first_at: String,
    /// The first paths, each once, in the order they came.
    paths: Vec<String>,
    /// Changes to further paths, beyond [`LOGGED_PATHS`].
    more: usize,
}

/// The paths a restart's log line names at most.
const LOGGED_PATHS: usize = 10;

impl Pending {
    fn add(&self, key: &str, rel: &str) {
        let mut all = self.0.lock().unwrap();
        let seen = all.entry(key.to_string()).or_insert_with(|| Seen {
            first: Instant::now(),
            first_at: crate::diag::now(),
            paths: Vec::new(),
            more: 0,
        });
        if seen.paths.iter().any(|p| p == rel) {
            return;
        }
        if seen.paths.len() < LOGGED_PATHS {
            seen.paths.push(rel.to_string());
        } else {
            seen.more += 1;
        }
    }

    /// What `key` waits for; `None` when a restart that ran meanwhile took it
    /// (and covers it: the change came before that restart's stop).
    fn take(&self, key: &str) -> Option<Seen> {
        self.0.lock().unwrap().remove(key)
    }
}

impl Seen {
    /// One line in the daemon log: `fields` with what changed, when the first
    /// change was seen, and how long ago that was.
    fn log(&self, event: &str, mut fields: serde_json::Value) {
        if let Some(f) = fields.as_object_mut() {
            f.insert("paths".into(), self.paths.clone().into());
            if self.more > 0 {
                f.insert("more_paths".into(), self.more.into());
            }
            f.insert("first_change".into(), self.first_at.clone().into());
            let waited = u64::try_from(self.first.elapsed().as_millis()).unwrap_or(u64::MAX);
            f.insert("waited_ms".into(), waited.into());
        }
        crate::diag::info(event, fields);
    }
}

/// The debounce for dev server `name`'s `restart_on` is over: restart it for
/// the changes `pending` holds under `key`.
///
/// A dev server that is starting or being restarted (for an earlier change,
/// or by `devstack restart`) may have read the tree before this change: the
/// restart waits until it is up, polled once per debounce, rather than
/// dropping the change while it has no running instance, as it did. One
/// that is stopped or failed is not started by a change.
fn restart_devserver(s: &Arc<Supervisor>, key: String, name: String, pending: Arc<Pending>) {
    let busy = s.restart_under_way(&name) || (s.service_active(&name) && !s.service_running(&name));
    if busy {
        s.debounce(key.clone(), move |s| {
            restart_devserver(s, key, name, pending)
        });
        return;
    }
    let Some(seen) = pending.take(&key) else {
        return;
    };
    let fields = serde_json::json!({ "lane": s.lane, "service": name });
    if s.service_running(&name) {
        seen.log("restart_on", fields);
        let _ = s.restart(&name);
    } else {
        let mut fields = fields;
        fields["why"] = "it is not running; a change does not start it".into();
        seen.log("restart_on_skipped", fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind, RenameMode};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[test]
    fn generated_directories_do_not_count() {
        for p in [
            "src/Api/bin/Debug/x.dll",
            "neo/src/app/node_modules/a/b.js",
            ".devstack/lanes/main/src/x.cs",
            "obj/project.assets.json",
            ".git/index",
        ] {
            assert!(super::ignored(p), "{p}");
        }
        for p in [
            "src/Api/Program.cs",
            "src/binary-tools/x.cs",
            "docs/objects.md",
            "src/cache/x.ts",
            "web/src/app/angular.json",
        ] {
            assert!(!super::ignored(p), "{p}");
        }
    }

    #[test]
    fn writes_in_progress_do_not_count() {
        for p in [
            "server/src/db.rs.tmp.33128.2dcc67a056d2",
            "schema/migrations/0045_icon.sql.tmp.33128.3fd2b0a603be",
            "tools/mock/sedGr1VFQ",
            "src/4913",
            "src/main.rs~",
            "src/.#main.rs",
            "src/.main.rs.swp",
        ] {
            assert!(super::ignored(p), "{p}");
        }
        for p in [
            "src/db.rs",
            "src/data.tmp",
            "src/v1.tmp.x.y",
            "src/sedan.rs",
            "src/seducers",
            "bin2/sedimentd",
            "docs/4913.md",
            "src/swap.swp",
        ] {
            assert!(!super::ignored(p), "{p}");
        }
    }

    #[test]
    fn frontend_caches_do_not_count_but_are_synced_to_a_remote_lane() {
        for p in [
            "web/.angular/cache/20.1.0/app/vite/deps/package.json",
            "web/.angular/cache/20.1.0/app/vite/deps_temp_5f3a/package.json",
            "web/.vite/deps/_metadata.json",
            ".next/cache/x",
            "web/.nuxt/x",
            ".turbo/x",
            "node_tools/.cache/x",
            "web/.svelte-kit/x",
            ".parcel-cache/x",
        ] {
            assert!(super::ignored(p), "{p}");
            assert!(!super::in_generated(p), "{p}");
        }
        assert!(super::in_generated("src/Api/bin/x.dll"));
    }

    #[test]
    fn python_caches_do_not_count() {
        for p in [
            "src/api/__pycache__/main.cpython-313.pyc",
            "__pycache__/x.pyc",
            ".pytest_cache/v/cache/nodeids",
            ".mypy_cache/3.13/x.json",
            "src/.ruff_cache/0.6.0/x",
        ] {
            assert!(super::ignored(p), "{p}");
            assert!(!super::in_generated(p), "{p}");
        }
        assert!(!super::ignored("src/api/pycache.py"));
    }

    #[test]
    fn restart_on_covers_what_a_folder_holds() {
        let g = set(&[
            "src/api".into(),
            "uv.lock".into(),
            "web/*.json".into(),
            "conf/**/*.yml".into(),
        ])
        .unwrap();
        for p in [
            "src/api",
            "src/api/probe.txt",
            "src/api/routes/users.py",
            "uv.lock",
            "web/angular.json",
            "conf/a/b.yml",
        ] {
            assert!(covers(&g, p), "{p}");
        }
        // What the watcher matched before (#221): the folder's own path only.
        assert!(!g.is_match("src/api/probe.txt"));
        for p in [
            "src",
            "src/apiclient/x.py",
            "src/other/api/x.py",
            "uv.lock.bak",
            "docs/uv.lock",
            "web/src/x.json",
            "conf/a/b.json",
        ] {
            assert!(!covers(&g, p), "{p}");
        }
    }

    /// The bug itself (#221) against the real OS watch: a file created,
    /// saved and deleted in a folder `restart_on` names is reported by the
    /// file's own path, at once, and counts for the folder. Windows reports
    /// the folder itself for some of them only, never for the save.
    #[test]
    fn a_file_created_saved_and_deleted_in_a_restart_on_folder_counts() {
        use notify::Watcher as _;
        use std::time::Duration;

        let d = tempfile::tempdir().unwrap();
        let repo = d.path().to_path_buf();
        let real = std::fs::canonicalize(&repo).unwrap();
        std::fs::create_dir_all(repo.join("src").join("api")).unwrap();
        let g = set(&["src/api".into()]).unwrap();
        let hits = Arc::new(Mutex::new(Vec::<String>::new()));
        let (sink, root) = (hits.clone(), repo.clone());
        let mut w = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(mut ev) = res else {
                return;
            };
            ev.paths = std::mem::take(&mut ev.paths)
                .into_iter()
                .map(|p| as_given(&root, &real, p))
                .collect();
            for rel in rels(&root, ev) {
                if covers(&g, &rel) {
                    sink.lock().unwrap().push(rel);
                }
            }
        })
        .unwrap();
        w.watch(&repo, RecursiveMode::Recursive).unwrap();
        std::thread::sleep(Duration::from_millis(300));

        let probe = repo.join("src").join("api").join("probe.txt");
        let steps: [(&str, &dyn Fn()); 3] = [
            ("created", &|| std::fs::write(&probe, "probe 1\n").unwrap()),
            ("saved", &|| {
                use std::io::Write as _;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&probe)
                    .unwrap();
                f.write_all(b"probe 2\n").unwrap();
            }),
            ("deleted", &|| std::fs::remove_file(&probe).unwrap()),
        ];
        for (what, step) in steps {
            // What came before (the creation's own modification) is not this step's.
            std::thread::sleep(Duration::from_millis(300));
            hits.lock().unwrap().clear();
            step();
            let mut seen = false;
            for _ in 0..200 {
                if hits
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p == "src/api/probe.txt")
                {
                    seen = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(seen, "a file {what}: {:?}", hits.lock().unwrap());
        }
    }

    #[test]
    fn a_restart_names_each_path_once_and_counts_the_rest() {
        let p = Pending::default();
        for rel in ["src/api/a.py", "src/api/b.py", "src/api/a.py"] {
            p.add("devserver:api", rel);
        }
        for i in 0..LOGGED_PATHS + 3 {
            p.add("devserver:web", &format!("web/{i}.ts"));
        }
        let api = p.take("devserver:api").unwrap();
        assert_eq!(api.paths, ["src/api/a.py", "src/api/b.py"]);
        assert_eq!(api.more, 0);
        let web = p.take("devserver:web").unwrap();
        assert_eq!(web.paths.len(), LOGGED_PATHS);
        assert_eq!(web.more, 3);
        assert!(
            p.take("devserver:api").is_none(),
            "taken by the restart that ran"
        );
    }

    #[test]
    fn a_rule_file_is_seen_by_its_raw_path() {
        let repo = Path::new("/r");
        for p in ["/r/.gitignore", "/r/web/.gitignore", "/r/.git/info/exclude"] {
            assert!(rule_file(repo, Path::new(p)), "{p}");
        }
        for p in ["/r/src/a.cs", "/elsewhere/.gitignore", "/r/.git/index"] {
            assert!(!rule_file(repo, Path::new(p)), "{p}");
        }
    }

    #[test]
    fn a_real_path_is_taken_back_under_the_repository() {
        // FSEvents: the repository opened as /tmp/r lives in /private/tmp/r.
        let (repo, real) = (Path::new("/tmp/r"), Path::new("/private/tmp/r"));
        assert_eq!(
            as_given(repo, real, PathBuf::from("/private/tmp/r/src/a.cs")),
            Path::new("/tmp/r/src/a.cs")
        );
        assert_eq!(
            as_given(repo, real, PathBuf::from("/tmp/r/src/a.cs")),
            Path::new("/tmp/r/src/a.cs")
        );
        assert_eq!(
            as_given(repo, real, PathBuf::from("/private/tmp/other/a.cs")),
            Path::new("/private/tmp/other/a.cs")
        );
        // Where the repository is its real path, nothing moves.
        assert_eq!(
            as_given(repo, repo, PathBuf::from("/tmp/r/a.cs")),
            Path::new("/tmp/r/a.cs")
        );
    }

    /// A tree that exists only as a map, and a record of every question asked
    /// of it.
    struct Spy {
        dirs: BTreeMap<PathBuf, Vec<(PathBuf, bool)>>,
        asked: RefCell<Vec<String>>,
    }

    impl Spy {
        /// `dirs`: each directory with its entries and whether each is a
        /// directory. An entry that is not a directory itself (a file, a
        /// link) has no row of its own.
        fn new(dirs: &[(&str, &[(&str, bool)])]) -> Self {
            Spy {
                dirs: dirs
                    .iter()
                    .map(|(d, es)| {
                        let es = es.iter().map(|(p, d)| (PathBuf::from(p), *d)).collect();
                        (PathBuf::from(d), es)
                    })
                    .collect(),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fs for Spy {
        fn is_dir(&self, path: &Path) -> bool {
            self.asked
                .borrow_mut()
                .push(format!("is_dir {}", path.display()));
            self.dirs.contains_key(path)
        }

        fn entries(&self, dir: &Path) -> Vec<(PathBuf, bool)> {
            self.asked
                .borrow_mut()
                .push(format!("entries {}", dir.display()));
            self.dirs.get(dir).cloned().unwrap_or_default()
        }
    }

    fn event(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn what_does_not_count_costs_no_file_system_call() {
        // `pnpm install` makes a thousand junctions in `node_modules` at once.
        let fs = Spy::new(&[
            (
                "/r/node_modules/.pnpm/node_modules/x",
                &[("/r/node_modules/.pnpm/node_modules/x/y.js", false)],
            ),
            ("/r/src/Api/bin", &[]),
            ("/elsewhere/dir", &[]),
            ("/r", &[]),
        ]);
        for p in [
            "/r/node_modules/.pnpm/node_modules/x",
            "/r/src/Api/bin",
            "/r/src/app/node_modules",
            "/r/.git/objects/ab",
            "/r/target/debug",
            "/r/.devstack/lanes/main/src",
            "/r/obj",
            "/r/web/.angular/cache/20/app/vite/deps_temp_1",
            "/elsewhere/dir", // not in the repository
            "/r",             // the repository itself
        ] {
            for kind in [
                EventKind::Create(CreateKind::Folder),
                EventKind::Create(CreateKind::Any),
                EventKind::Modify(ModifyKind::Any),
                EventKind::Remove(RemoveKind::Any),
            ] {
                assert!(
                    event_paths(Path::new("/r"), event(kind, p), &fs, true).is_empty(),
                    "{p}"
                );
            }
        }
        assert!(fs.asked.borrow().is_empty(), "{:?}", fs.asked);
    }

    #[test]
    fn a_new_directory_is_reported_with_what_it_holds() {
        let fs = Spy::new(&[
            (
                "/r/src/new",
                &[
                    ("/r/src/new/a.cs", false),
                    ("/r/src/new/sub", true),
                    ("/r/src/new/node_modules", true),
                    ("/r/src/new/link", false), // a link to a directory: a leaf
                ],
            ),
            (
                "/r/src/new/sub",
                &[("/r/src/new/sub/b.cs", false), ("/r/src/new/sub/bin", true)],
            ),
            (
                "/r/src/new/node_modules",
                &[("/r/src/new/node_modules/x.js", false)],
            ),
            ("/r/src/new/sub/bin", &[("/r/src/new/sub/bin/y.dll", false)]),
        ]);
        // `Any` is what ReadDirectoryChangesW says, `Folder` what inotify says.
        for kind in [CreateKind::Any, CreateKind::Folder] {
            fs.asked.borrow_mut().clear();
            let ev = event(EventKind::Create(kind), "/r/src/new");
            let mut got = event_paths(Path::new("/r"), ev, &fs, true);
            got.sort();
            assert_eq!(
                got,
                [
                    "src/new",
                    "src/new/a.cs",
                    "src/new/link",
                    "src/new/sub/b.cs"
                ]
            );
            assert_eq!(
                *fs.asked.borrow(),
                [
                    "is_dir /r/src/new",
                    "entries /r/src/new",
                    "entries /r/src/new/sub"
                ],
                "generated directories are neither listed nor entered"
            );
        }
    }

    #[test]
    fn without_listing_a_new_directory_is_never_opened() {
        let fs = Spy::new(&[("/r/src/new", &[("/r/src/new/a.cs", false)])]);
        let ev = event(EventKind::Create(CreateKind::Any), "/r/src/new");
        assert_eq!(event_paths(Path::new("/r"), ev, &fs, false), ["src/new"]);
        assert!(fs.asked.borrow().is_empty(), "{:?}", fs.asked);
    }

    /// Windows has to stay on `false`: the guard that does not depend on timing.
    #[cfg(windows)]
    #[test]
    fn windows_never_asks_the_file_system_about_a_new_directory() {
        let fs = Spy::new(&[("/r/src/new", &[("/r/src/new/a.cs", false)])]);
        for kind in [CreateKind::Any, CreateKind::Folder] {
            let ev = event(EventKind::Create(kind), "/r/src/new");
            assert_eq!(
                event_paths(Path::new("/r"), ev, &fs, LIST_NEW_DIRS),
                ["src/new"]
            );
        }
        assert!(fs.asked.borrow().is_empty(), "{:?}", fs.asked);
    }

    /// What `LIST_NEW_DIRS` on Windows rests on, against the real OS watch:
    /// what is created in a new directory is reported itself; a directory that
    /// arrives populated, moved in from outside the tree or from one folder of
    /// it to another, is reported without its contents.
    #[cfg(windows)]
    #[test]
    fn windows_reports_creations_but_not_the_contents_of_a_move() {
        use notify::Watcher as _;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap(); // the same volume
        let repo = d.path().to_path_buf();
        std::fs::create_dir(repo.join("src")).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let (sink, root) = (seen.clone(), repo.clone());
        let mut w = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(ev) = res
                && matches!(ev.kind, EventKind::Create(_))
            {
                for p in ev.paths {
                    sink.lock().unwrap().extend(counted(&root, &p));
                }
            }
        })
        .unwrap();
        w.watch(&repo, RecursiveMode::Recursive).unwrap();

        let pop = outside.path().join("pop");
        std::fs::create_dir_all(pop.join("sub")).unwrap();
        std::fs::write(pop.join("a.cs"), "").unwrap();
        std::fs::write(pop.join("sub").join("b.cs"), "").unwrap();
        std::fs::create_dir(repo.join("src").join("made")).unwrap();
        std::fs::write(repo.join("src").join("made").join("x.cs"), "").unwrap();
        std::fs::rename(&pop, repo.join("src").join("pop")).unwrap();

        let want = ["src/made", "src/made/x.cs", "src/pop"];
        for _ in 0..200 {
            let s = seen.lock().unwrap();
            if want.iter().all(|w| s.iter().any(|p| p == w)) {
                break;
            }
            drop(s);
            std::thread::sleep(Duration::from_millis(25));
        }
        // Whatever of the moved-in directory would be reported shows by now.
        std::thread::sleep(Duration::from_millis(300));
        let s = seen.lock().unwrap().clone();
        for w in want {
            assert!(s.iter().any(|p| p == w), "{w} in {s:?}");
        }
        let hint = "its files are reported now: reconsider LIST_NEW_DIRS";
        assert!(
            !s.iter().any(|p| p.starts_with("src/pop/")),
            "{hint}: {s:?}"
        );

        // The same from one folder of the tree to another.
        std::fs::create_dir(repo.join("src").join("dest")).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        seen.lock().unwrap().clear();
        std::fs::rename(
            repo.join("src").join("pop"),
            repo.join("src").join("dest").join("pop"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let s = seen.lock().unwrap().clone();
        assert!(s.iter().any(|p| p == "src/dest/pop"), "{s:?}");
        assert!(
            !s.iter().any(|p| p.starts_with("src/dest/pop/")),
            "{hint}: {s:?}"
        );
    }

    /// The bug itself, against the real OS watch and the real event logic: a
    /// tool that makes a directory and then opens it with no sharing (what
    /// libuv does for a junction) while the watcher runs. Listing each new
    /// directory made one creation in thirty fail, in `node_modules` and
    /// anywhere else; not opening them makes none. A regression shows in most
    /// runs, not in all: the test above is the exact guard.
    #[cfg(windows)]
    #[test]
    fn an_exclusive_open_of_a_new_directory_is_not_disturbed() {
        use notify::Watcher as _;
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let d = tempfile::tempdir().unwrap();
        let repo = d.path().to_path_buf();
        let seen = Arc::new(AtomicUsize::new(0));
        let (root, count) = (repo.clone(), seen.clone());
        let mut w = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(ev) = res {
                count.fetch_add(1, Ordering::Relaxed);
                rels(&root, ev);
            }
        })
        .unwrap();
        w.watch(&repo, RecursiveMode::Recursive).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));

        for under in ["node_modules/.pnpm/node_modules", "pkgs"] {
            let base = repo.join(under);
            std::fs::create_dir_all(&base).unwrap();
            let (next, failed) = (AtomicUsize::new(0), AtomicUsize::new(0));
            std::thread::scope(|s| {
                for _ in 0..4 {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= 3000 {
                                break;
                            }
                            let dir = base.join(format!("p{i}"));
                            std::fs::create_dir(&dir).unwrap();
                            let open = std::fs::OpenOptions::new()
                                .access_mode(0x4000_0000) // GENERIC_WRITE
                                .share_mode(0)
                                // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
                                .custom_flags(0x0200_0000 | 0x0020_0000)
                                .open(&dir);
                            if open.is_err() {
                                failed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                }
            });
            assert_eq!(failed.into_inner(), 0, "{under}");
        }
        assert!(
            seen.load(Ordering::Relaxed) > 1000,
            "the watch saw almost nothing"
        );
    }

    #[test]
    fn only_a_creation_lists_a_directory() {
        let fs = Spy::new(&[("/r/src/new", &[("/r/src/new/a.cs", false)])]);
        for kind in [
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            EventKind::Remove(RemoveKind::Folder),
        ] {
            let got = event_paths(Path::new("/r"), event(kind, "/r/src/new"), &fs, true);
            assert_eq!(got, ["src/new"]);
        }
        let open = event(EventKind::Access(AccessKind::Any), "/r/src/new");
        assert!(event_paths(Path::new("/r"), open, &fs, true).is_empty());
        assert!(fs.asked.borrow().is_empty(), "{:?}", fs.asked);
    }

    /// A link to the directory `target`, at `link`: a symlink, or a junction
    /// on Windows (which needs no privilege, and is what `pnpm` makes).
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            let out = std::process::Command::new("cmd")
                .raw_arg(format!(
                    "/D /S /C \"mklink /J \"{}\" \"{}\"\"",
                    link.display(),
                    target.display()
                ))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn the_disk_listing_skips_generated_directories_and_links() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let new = repo.join("src").join("new");
        std::fs::create_dir_all(new.join("sub")).unwrap();
        std::fs::create_dir_all(new.join("node_modules").join("p")).unwrap();
        std::fs::write(new.join("a.cs"), "").unwrap();
        std::fs::write(new.join("sub").join("b.cs"), "").unwrap();
        std::fs::write(new.join("node_modules").join("p").join("i.js"), "").unwrap();
        // A way back up to the repository: followed, the walk would start over
        // from the top below every new directory.
        let up = new.join("up");
        link_dir(repo, &up);

        let ev = Event::new(EventKind::Create(CreateKind::Any)).add_path(new.clone());
        let mut got = event_paths(repo, ev, &Disk, true);
        got.sort();
        assert_eq!(
            got,
            ["src/new", "src/new/a.cs", "src/new/sub/b.cs", "src/new/up"]
        );
        // Created as a link itself, it is not a directory to list.
        let ev = Event::new(EventKind::Create(CreateKind::Any)).add_path(up.clone());
        assert_eq!(event_paths(repo, ev, &Disk, true), ["src/new/up"]);

        std::fs::remove_dir(&up)
            .or_else(|_| std::fs::remove_file(&up))
            .unwrap();
    }
}
