//! The store of what `prepare` makes, warmed for refs before a lane needs
//! it (ADR-0065). devstack watches the refs of `[prepare] warm`, it never
//! fetches them; a new commit, unchanged for [`QUIET`], is checked out
//! beside the repository with an index of its own, and each service's
//! `prepare` with `outputs` is made into the store unless the store has its
//! key already.

use crate::state::NoticeLevel;
use crate::supervisor::{Supervisor, Warmed};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the refs are read.
const EVERY: Duration = Duration::from_secs(60);
/// How long a ref's new commit stays before it is warmed: a fetch, a pull,
/// a merge done.
const QUIET: Duration = Duration::from_secs(120);

/// [`EVERY`], or `DEVSTACK_WARM_EVERY_MS` (tests).
pub(crate) fn every() -> Duration {
    from_env("DEVSTACK_WARM_EVERY_MS").unwrap_or(EVERY)
}

/// [`QUIET`], or `DEVSTACK_WARM_QUIET_MS` (tests).
fn quiet() -> Duration {
    from_env("DEVSTACK_WARM_QUIET_MS").unwrap_or(QUIET)
}

fn from_env(k: &str) -> Option<Duration> {
    std::env::var(k)
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_millis)
}

/// In a warm lease's container: what its `prepare` runs did, once done.
static LANE: std::sync::Mutex<Option<Vec<serde_json::Value>>> = std::sync::Mutex::new(None);

/// This daemon runs a warm lease on a node (`DEVSTACK_WARM`, ADR-0065
/// point 5).
pub(crate) fn warm_lease() -> bool {
    crate::remote_lane() && std::env::var_os("DEVSTACK_WARM").is_some_and(|v| v == "1")
}

/// In a warm lease's container: its `prepare` runs, in the background; what
/// they did is `GET /v1/warm`'s once done.
pub(crate) fn run_lane(sup: Arc<Supervisor>) {
    let _ = std::thread::Builder::new()
        .name("warm".into())
        .spawn(move || {
            let done = sup.warm_lane(&|| sup.closed());
            *LANE.lock().unwrap() = Some(done);
        });
}

/// `GET /v1/warm`: whether this daemon runs a warm lease, and what its
/// `prepare` runs did once they are done.
pub(crate) fn lane_view() -> serde_json::Value {
    let done = LANE.lock().unwrap().clone();
    serde_json::json!({ "warm": warm_lease(), "done": done.is_some(),
        "results": done.unwrap_or_default() })
}

/// What a daemon saw of the refs: each one's commit, and since when.
#[derive(Default)]
pub(crate) struct Watch {
    seen: BTreeMap<String, (String, Instant)>,
}

impl Watch {
    /// The refs to warm now, with their commit: new, quiet for long enough,
    /// and not warmed before (the note survives a daemon's restart).
    pub(crate) fn due(&mut self, sup: &Supervisor) -> Vec<(String, String)> {
        let done = load_done(&done_file(sup));
        let mut out = Vec::new();
        for r in &sup.stack.prepare.warm {
            let Some(commit) = rev_parse(&sup.repo, r) else {
                continue;
            };
            let e = self
                .seen
                .entry(r.clone())
                .or_insert_with(|| (commit.clone(), Instant::now()));
            if e.0 != commit {
                *e = (commit.clone(), Instant::now());
            }
            if e.1.elapsed() >= quiet() && done.get(r) != Some(&commit) {
                out.push((r.clone(), commit));
            }
        }
        out
    }
}

/// Where the commits warmed last are noted, by ref.
fn done_file(sup: &Supervisor) -> PathBuf {
    sup.state_dir.join("warm").join("done.json")
}

fn load_done(file: &Path) -> BTreeMap<String, String> {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// `r`'s commit was warmed: not again, not after a restart either.
pub(crate) fn note_done(sup: &Supervisor, r: &str, commit: &str) {
    let file = done_file(sup);
    let mut done = load_done(&file);
    done.insert(r.to_string(), commit.to_string());
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, serde_json::to_vec_pretty(&done).unwrap_or_default());
}

/// The commit `r` names, if it names one.
fn rev_parse(repo: &Path, r: &str) -> Option<String> {
    let out = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let c = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !c.is_empty()).then_some(c)
}

/// A commit checked out beside the repository as git checks it out (line
/// endings, filters), with an index of its own: the main tree's index,
/// worktree and list of worktrees stay as they are. Removed when dropped.
pub(crate) struct Checkout {
    repo: PathBuf,
    dir: PathBuf,
    index: PathBuf,
    /// The checked-out files: what a copy of the ref would hold.
    pub src: PathBuf,
}

impl Checkout {
    pub(crate) fn new(sup: &Supervisor, r: &str, commit: &str, tag: &str) -> Result<Self, String> {
        let slug: String = r
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = sup
            .state_dir
            .join("warm")
            .join(format!("{slug}-{}{tag}", &commit[..commit.len().min(12)]));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).map_err(|e| e.to_string())?;
        let co = Self {
            repo: sup.repo.clone(),
            index: dir.join("index"),
            src: dir.join("src"),
            dir,
        };
        co.git(&["read-tree", commit], None)?;
        Ok(co)
    }

    /// `git` in the repository with this checkout's index; `input` on stdin.
    fn git(&self, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .env("GIT_INDEX_FILE", &self.index)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("git: {e}"))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input).map_err(|e| e.to_string())?;
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The commit's files, as paths below the repository.
    pub(crate) fn files(&self) -> Result<Vec<String>, String> {
        Ok(self
            .git(&["ls-files", "-z"], None)?
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn prefix(&self) -> String {
        format!("{}/", self.src.to_string_lossy().replace('\\', "/"))
    }

    /// `files` of the commit checked out.
    pub(crate) fn check_out(&self, files: &[&String]) -> Result<(), String> {
        if files.is_empty() {
            return Ok(());
        }
        let mut list = Vec::new();
        for f in files {
            list.extend_from_slice(f.as_bytes());
            list.push(0);
        }
        let prefix = format!("--prefix={}", self.prefix());
        self.git(
            &["checkout-index", "-f", "-z", "--stdin", &prefix],
            Some(&list),
        )
        .map(|_| ())
    }

    /// What completes the checkout, once, for `prepare` to run in it.
    pub(crate) fn completer(&self) -> Box<dyn Fn() -> Result<(), String> + Send> {
        let (repo, index, prefix, mark) = (
            self.repo.clone(),
            self.index.clone(),
            self.prefix(),
            self.dir.join("complete"),
        );
        Box::new(move || {
            if mark.is_file() {
                return Ok(());
            }
            let out = Command::new("git")
                .args(["checkout-index", "-a", "-f", &format!("--prefix={prefix}")])
                .current_dir(&repo)
                .env("GIT_INDEX_FILE", &index)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("git: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "git checkout-index: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            std::fs::write(&mark, "").map_err(|e| e.to_string())
        })
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Warm the store here for commit `commit` of ref `r` (ADR-0065 points 3,
/// 4): each service whose `prepare` names `outputs`, its inputs checked out
/// first, the rest only for a key the store lacks. Said in the diagnostic
/// log, a failure as a notice once per commit. The checkout, for the nodes.
pub(crate) fn warm_here(
    sup: &Arc<Supervisor>,
    r: &str,
    commit: &str,
    cancel: &dyn Fn() -> bool,
) -> Result<Checkout, String> {
    let co = Checkout::new(sup, r, commit, "")?;
    let files = co.files()?;
    for (name, svc) in &sup.stack.services {
        let Some(p) = svc.prepare.as_ref().filter(|p| !p.outputs.is_empty()) else {
            continue;
        };
        if cancel() {
            break;
        }
        let globs = devstack_build::watch_set(&p.inputs).ok().flatten();
        let inputs: Vec<&String> = files
            .iter()
            .filter(|f| globs.as_ref().is_some_and(|g| g.is_match(f.as_str())))
            .collect();
        let id = format!("prepare.warm.{name}");
        let warmed = co
            .check_out(&inputs)
            .and_then(|()| sup.warm_service(name, svc, &co.src, co.completer(), cancel));
        match warmed {
            Ok(w) => {
                sup.clear_notice(&id);
                let (key, ms, had) = match w {
                    Warmed::Had(k) => (k, 0, true),
                    Warmed::Made(k, d) => (k, d.as_millis() as u64, false),
                };
                crate::diag::info(
                    "prepare_warmed",
                    serde_json::json!({ "service": name, "key": key, "ref": r,
                        "commit": commit, "place": "local", "had": had, "ms": ms }),
                );
            }
            Err(e) if cancel() => return Err(e),
            Err(e) => {
                crate::diag::warn(
                    "prepare_warm_failed",
                    serde_json::json!({ "service": name, "ref": r, "commit": commit,
                        "place": "local", "error": e }),
                );
                sup.set_notice(
                    &id,
                    NoticeLevel::Warn,
                    format!(
                        "`prepare` of {name} could not be warmed for `{r}` ({}): {e}. The first lane on that commit prepares itself.",
                        &commit[..commit.len().min(12)]
                    ),
                );
            }
        }
    }
    Ok(co)
}
