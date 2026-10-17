import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'commands.rs',[
("""use std::collections::BTreeMap;""","""use std::collections::{BTreeMap, BTreeSet};"""),
("""#[derive(Default)]
pub struct Runs {
    next: AtomicU64,
    runs: Mutex<BTreeMap<u64, RunInfo>>,
}""",
"""#[derive(Default)]
pub struct Runs {
    next: AtomicU64,
    runs: Mutex<BTreeMap<u64, RunInfo>>,
    /// Runs asked to stop (`POST /v1/runs/{id}/cancel`).
    cancelled: Mutex<BTreeSet<u64>>,
    /// The job a run's `run` step runs now, by run: a cancel stops it.
    jobs: Mutex<BTreeMap<u64, String>>,
}"""),
("""    fn update(&self, id: u64, f: impl FnOnce(&mut RunInfo)) {
        if let Some(r) = self.runs.lock().unwrap().get_mut(&id) {
            f(r);
        }
    }
}""",
"""    fn update(&self, id: u64, f: impl FnOnce(&mut RunInfo)) {
        if let Some(r) = self.runs.lock().unwrap().get_mut(&id) {
            f(r);
        }
    }

    fn is_cancelled(&self, id: u64) -> bool {
        self.cancelled.lock().unwrap().contains(&id)
    }

    /// Stop run `id`: an `exec` step's process tree is killed, a `run`
    /// step's job stopped; other steps finish, and no further step starts.
    /// `Ok(false)`: it is not running (any more). Blocking.
    pub fn cancel(&self, sup: &Supervisor, id: u64) -> Result<bool, String> {
        match self.get(id) {
            None => return Err(format!("no run {id}")),
            Some(r) if r.status != "running" => return Ok(false),
            Some(_) => {}
        }
        self.cancelled.lock().unwrap().insert(id);
        let job = self.jobs.lock().unwrap().get(&id).cloned();
        if let Some(job) = job {
            sup.stop(&job)?;
        }
        Ok(true)
    }
}"""),
("""    let (sup, runs, name) = (Arc::clone(sup), Arc::clone(runs), name.to_string());
    std::thread::spawn(move || {
        let result = execute(&sup, &cmd.steps, &log);""",
"""    let (sup, runs, name) = (Arc::clone(sup), Arc::clone(runs), name.to_string());
    std::thread::spawn(move || {
        let result = execute(&sup, &runs, id, &cmd.steps, &log);
        runs.cancelled.lock().unwrap().remove(&id);
        runs.jobs.lock().unwrap().remove(&id);"""),
("""fn execute(
    sup: &Arc<Supervisor>,
    steps: &[devstack_config::model::Step],
    log: &PathBuf,
) -> Result<(), String> {""",
"""/// The error of a run that was cancelled.
const CANCELLED: &str = "cancelled";

fn execute(
    sup: &Arc<Supervisor>,
    runs: &Runs,
    id: u64,
    steps: &[devstack_config::model::Step],
    log: &PathBuf,
) -> Result<(), String> {
    // In a lane, a command runs in its copy, never in the worktree (ADR-0024
    // point 2: git must be able to remove a worktree at any time; a process
    // in it would hold it open on Windows). The copy follows the worktree
    // first. `{repo}` is that copy, as for the lane's services.
    sup.sync_copy()?;
    let root = sup.run_root();"""),
("""    let ctx = CommandContext {
        repo: sup.repo.clone(),
        urls,
        lane: sup.lane.clone(),
    };""",
"""    let ctx = CommandContext {
        repo: root.clone(),
        urls,
        lane: sup.lane.clone(),
    };"""),
("""    for (i, step) in steps.iter().enumerate() {
        let action = step.action().map_err(|_| format!("step {i} is invalid"))?;""",
"""    for (i, step) in steps.iter().enumerate() {
        if runs.is_cancelled(id) {
            let _ = writeln!(out, "--- {CANCELLED}");
            return Err(CANCELLED.into());
        }
        let action = step.action().map_err(|_| format!("step {i} is invalid"))?;"""),
("""                let cwd = match cwd {
                    Some(c) => sup
                        .repo
                        .join(expand(c, &ctx, &env).map_err(|e| e.to_string())?),
                    None => sup.repo.clone(),
                };""",
"""                let cwd = match cwd {
                    Some(c) => root.join(expand(c, &ctx, &env).map_err(|e| e.to_string())?),
                    None => root.clone(),
                };"""),
("""                let code = loop {
                    if let Some(c) = tree.try_exit_code() {
                        break c;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                if code == 0 {
                    Ok(())
                } else {
                    Err(format!("step {} exited with code {code}", i + 1))
                }""",
"""                // `timeout` bounds an `exec` step too (none: no limit); a
                // cancel kills it at once.
                let t0 = std::time::Instant::now();
                let code = loop {
                    if let Some(c) = tree.try_exit_code() {
                        break Ok(c);
                    }
                    if runs.is_cancelled(id) {
                        tree.kill();
                        break Err(CANCELLED.to_string());
                    }
                    if let Some(t) = step.timeout
                        && t0.elapsed() >= t
                    {
                        tree.kill();
                        break Err(format!(
                            "step {} still ran after {} s (its `timeout`), and was stopped",
                            i + 1,
                            t.as_secs()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                match code {
                    Ok(0) => Ok(()),
                    Ok(code) => Err(format!("step {} exited with code {code}", i + 1)),
                    Err(e) => Err(e),
                }"""),
("""            StepAction::Run(s) => sup.stop(s).and_then(|_| sup.start(s)),
        };""",
"""            StepAction::Run(s) => {
                runs.jobs.lock().unwrap().insert(id, s.to_string());
                let r = sup.stop(s).and_then(|_| sup.start(s));
                runs.jobs.lock().unwrap().remove(&id);
                match r {
                    Err(_) if runs.is_cancelled(id) => Err(CANCELLED.into()),
                    r => r,
                }
            }
        };"""),
])
print("ok")
