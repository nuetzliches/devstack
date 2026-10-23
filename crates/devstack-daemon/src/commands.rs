//! Runs of the stack file's `commands` (plan §4.4, §6): steps in order, every
//! run with an id, its own log and a final status, kept for the history.

use crate::state::{EventBody, now_ms};
use crate::supervisor::Supervisor;
use devstack_config::interpolate::{Placeholder, Resolve, expand};
use devstack_config::model::StepAction;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub id: u64,
    pub command: String,
    pub started: u128,
    pub finished: Option<u128>,
    /// `running`, `succeeded`, `failed`
    pub status: String,
    pub error: Option<String>,
    pub log: PathBuf,
}

#[derive(Default)]
pub struct Runs {
    next: AtomicU64,
    runs: Mutex<BTreeMap<u64, RunInfo>>,
    /// Runs asked to stop (`POST /v1/runs/{id}/cancel`).
    cancelled: Mutex<BTreeSet<u64>>,
    /// The job a run's `run` step runs now, by run: a cancel stops it.
    jobs: Mutex<BTreeMap<u64, String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("no command `{0}`")]
    Unknown(String),
    #[error("{0}")]
    NeedsConfirmation(String),
    #[error("service `{0}` must be healthy for this command")]
    NotReady(String),
}

impl Runs {
    pub fn list(&self, command: Option<&str>) -> Vec<RunInfo> {
        self.runs
            .lock()
            .unwrap()
            .values()
            .filter(|r| command.is_none_or(|c| r.command == c))
            .cloned()
            .collect()
    }
    pub fn get(&self, id: u64) -> Option<RunInfo> {
        self.runs.lock().unwrap().get(&id).cloned()
    }
    fn update(&self, id: u64, f: impl FnOnce(&mut RunInfo)) {
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
}

/// Placeholders a command may use: repository, service URLs, the
/// published addresses of Compose services and the groups' projects.
struct CommandContext {
    repo: PathBuf,
    urls: BTreeMap<String, String>,
    published: BTreeMap<(String, String, u16), String>,
    projects: BTreeMap<String, String>,
    lane: String,
}

impl Resolve for CommandContext {
    fn resolve(&self, p: &Placeholder) -> Result<String, String> {
        match p {
            Placeholder::Repo => Ok(self.repo.to_string_lossy().into_owned()),
            Placeholder::Lane => Ok(devstack_env::lane_name(&self.lane).to_string()),
            Placeholder::LaneSuffix => Ok(devstack_env::lane_suffix(&self.lane)),
            Placeholder::ServiceUrl(s) => self
                .urls
                .get(s)
                .cloned()
                .ok_or_else(|| format!("service `{s}` has no URL")),
            Placeholder::ServicePort(s) => self
                .urls
                .get(s)
                .and_then(|u| u.rsplit(':').next())
                .map(str::to_string)
                .ok_or_else(|| format!("service `{s}` has no port")),
            Placeholder::ComposePort {
                group,
                service,
                port,
                part,
            } => self
                .published
                .get(&(group.clone(), service.clone(), *port))
                .map(|a| part.of(a))
                .ok_or_else(|| format!("port {port} of `{group}/{service}` is not published")),
            Placeholder::ComposeProject(group) => self
                .projects
                .get(group)
                .cloned()
                .ok_or_else(|| format!("no compose group `{group}`")),
            other => Err(format!("`{other}` is not available in a command")),
        }
    }
}

/// A `start`, `stop` or `restart` step on a service that runs in the main
/// working tree only (`lanes = false`) has nothing to act on in a lane: the
/// main tree's instance serves it. The step is skipped, and the run's log
/// says why, as `after` skips such a service: a command written for both
/// trees does not end as failed in a lane after everything it needed ran.
fn main_tree_only(
    stack: &devstack_config::Stack,
    lane: &str,
    action: &StepAction,
) -> Option<String> {
    let (StepAction::Start(name) | StepAction::Stop(name) | StepAction::Restart(name)) = action
    else {
        return None;
    };
    (!lane.is_empty() && stack.services.get(*name).is_some_and(|s| !s.lanes)).then(|| {
        format!(
            "`{name}` runs in the main working tree only (`lanes = false`); its instance there serves this lane"
        )
    })
}

/// Run logs kept per command.
const KEEP_COMMAND_LOGS: usize = 20;

/// Start a run in the background; returns its id.
pub fn start(
    sup: &Arc<Supervisor>,
    runs: &Arc<Runs>,
    name: &str,
    confirmed: bool,
) -> Result<u64, RunError> {
    let cmd = sup
        .stack
        .commands
        .get(name)
        .ok_or_else(|| RunError::Unknown(name.into()))?
        .clone();
    if let Some(text) = &cmd.confirm
        && !confirmed
    {
        return Err(RunError::NeedsConfirmation(text.clone()));
    }
    let view = sup.view();
    for r in &cmd.requires {
        if !view
            .services
            .iter()
            .any(|s| &s.name == r && s.status.is_ready())
        {
            return Err(RunError::NotReady(r.clone()));
        }
    }
    let id = runs.next.fetch_add(1, Ordering::SeqCst) + 1;
    let log = sup
        .state_dir
        .join("logs")
        .join("commands")
        .join(name)
        .join(format!("{}-{id}.log", now_ms()));
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    crate::diag::prune(log.parent().unwrap(), "", KEEP_COMMAND_LOGS);
    runs.runs.lock().unwrap().insert(
        id,
        RunInfo {
            id,
            command: name.into(),
            started: now_ms(),
            finished: None,
            status: "running".into(),
            error: None,
            log: log.clone(),
        },
    );
    sup.emit(EventBody::Command {
        command: name.into(),
        run: id,
        status: "running".into(),
    });

    let (sup, runs, name) = (Arc::clone(sup), Arc::clone(runs), name.to_string());
    let t0 = std::time::Instant::now();
    std::thread::spawn(move || {
        let result = execute(&sup, &runs, id, &cmd.steps, &log);
        runs.cancelled.lock().unwrap().remove(&id);
        runs.jobs.lock().unwrap().remove(&id);
        let status = if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        };
        // Its time, and why it failed (the diagnostic line; only its first
        // line leaves the machine, ADR-0037).
        let mut extra = serde_json::Map::new();
        extra.insert("ms".into(), (t0.elapsed().as_millis() as u64).into());
        if let Err(e) = &result {
            extra.insert("error".into(), e.clone().into());
        }
        runs.update(id, |r| {
            r.finished = Some(now_ms());
            r.status = status.into();
            r.error = result.err();
        });
        sup.emit_with(
            EventBody::Command {
                command: name,
                run: id,
                status: status.into(),
            },
            Some(extra),
        );
    });
    Ok(id)
}

/// The error of a run that was cancelled.
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
    let root = sup.run_root();
    let mut urls = BTreeMap::new();
    for (n, s) in &sup.stack.services {
        if let Some(p) = s.port {
            urls.insert(n.clone(), format!("{}://127.0.0.1:{p}", s.scheme()));
        }
    }
    let ctx = CommandContext {
        repo: root.clone(),
        urls,
        published: sup.published_ports(),
        projects: sup.compose_projects(),
        lane: sup.lane.clone(),
    };
    let vars = devstack_env::variables(&sup.stack, None, &sup.repo);
    let env = |k: &str| vars.get(k).cloned();
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| e.to_string())?;
    for (i, step) in steps.iter().enumerate() {
        if runs.is_cancelled(id) {
            let _ = writeln!(out, "--- {CANCELLED}");
            return Err(CANCELLED.into());
        }
        let action = step.action().map_err(|_| format!("step {i} is invalid"))?;
        let _ = writeln!(out, "--- step {} : {action:?}", i + 1);
        if let Some(why) = main_tree_only(&sup.stack, &sup.lane, &action) {
            let _ = writeln!(out, "--- skipped: {why}");
            continue;
        }
        let r = match action {
            StepAction::Exec { command, cwd } => {
                let command = expand(command, &ctx, &env).map_err(|e| e.to_string())?;
                let cwd = match cwd {
                    Some(c) => root.join(expand(c, &ctx, &env).map_err(|e| e.to_string())?),
                    None => root.clone(),
                };
                let file = out.try_clone().map_err(|e| e.to_string())?;
                // A profile's variables reach its commands too (ADR-0041),
                // with the tree's own addresses.
                let mut step_env = BTreeMap::new();
                for (k, v) in sup.stack.profile.iter().flat_map(|p| &p.env) {
                    step_env.insert(k.clone(), expand(v, &ctx, &env).map_err(|e| e.to_string())?);
                }
                let spec = devstack_proc::Spec {
                    name: "command".into(),
                    command,
                    cwd,
                    env: step_env,
                    log: file,
                };
                let (mut tree, _) =
                    devstack_proc::spawn(spec, sup.proc_options()).map_err(|e| e.to_string())?;
                // `timeout` bounds an `exec` step too (none: no limit); a
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
                }
            }
            StepAction::Http { method, url } => {
                let url = expand(url, &ctx, &env).map_err(|e| e.to_string())?;
                let agent = ureq::Agent::config_builder()
                    .timeout_global(Some(step.timeout.unwrap_or(Duration::from_secs(30))))
                    .http_status_as_error(false)
                    .build()
                    .new_agent();
                let mut req = ureq::http::Request::builder().method(method).uri(&url);
                for (k, v) in &step.headers {
                    req = req.header(k, expand(v, &ctx, &env).map_err(|e| e.to_string())?);
                }
                let sent = match &step.body {
                    Some(b) => {
                        let b = expand(b, &ctx, &env).map_err(|e| e.to_string())?;
                        agent.run(req.body(b).map_err(|e| e.to_string())?)
                    }
                    None => agent.run(req.body(()).map_err(|e| e.to_string())?),
                };
                match sent {
                    Ok(mut resp) => {
                        let status = resp.status();
                        let body = resp.body_mut().read_to_string().unwrap_or_default();
                        let _ = writeln!(out, "{method} {url} -> {status}\n{body}");
                        if status.is_success() {
                            Ok(())
                        } else {
                            Err(format!("{method} {url} answered {status}"))
                        }
                    }
                    Err(e) => Err(format!("{method} {url}: {e}")),
                }
            }
            StepAction::Start(s) => sup.start(s),
            StepAction::Stop(s) => sup.stop(s),
            StepAction::Restart(s) => sup.restart(s),
            StepAction::Run(s) => {
                runs.jobs.lock().unwrap().insert(id, s.to_string());
                let r = sup.stop(s).and_then(|_| sup.start(s));
                runs.jobs.lock().unwrap().remove(&id);
                match r {
                    Err(_) if runs.is_cancelled(id) => Err(CANCELLED.into()),
                    r => r,
                }
            }
        };
        if let Err(e) = r {
            let _ = writeln!(out, "--- failed: {e}");
            return Err(e);
        }
    }
    let _ = writeln!(out, "--- done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STACK: &str = r#"
version = 1
[stack]
name = "s"
[compose.app]
project = "shop-dev"
files = ["compose.yml"]
services = ["db"]
[services.api]
run = "x"
[services.worker]
run = "x"
lanes = false
"#;

    fn stack() -> devstack_config::Stack {
        devstack_config::from_str(STACK, std::path::Path::new("t")).unwrap()
    }

    #[test]
    fn a_main_tree_only_service_is_skipped_in_a_lane() {
        let stack = stack();
        for action in [
            StepAction::Start("worker"),
            StepAction::Stop("worker"),
            StepAction::Restart("worker"),
        ] {
            let why = main_tree_only(&stack, "feat-x", &action);
            assert!(
                why.as_deref()
                    .is_some_and(|w| w.contains("main working tree only")),
                "{action:?}: {why:?}"
            );
            assert_eq!(main_tree_only(&stack, "", &action), None, "the main tree");
        }
        // A service a lane runs, and a `run` step, are acted on.
        assert_eq!(
            main_tree_only(&stack, "feat-x", &StepAction::Start("api")),
            None
        );
        assert_eq!(
            main_tree_only(&stack, "feat-x", &StepAction::Run("worker")),
            None
        );
    }

    #[test]
    fn a_groups_project_is_the_one_this_tree_runs() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (lane, project) in [("", "shop-dev"), ("feat-x", "shop-dev-feat-x")] {
            let d = tempfile::tempdir().unwrap();
            let file = d.path().join("devstack.toml");
            let state = d.path().join(".devstack");
            let sup = if lane.is_empty() {
                Supervisor::new(
                    stack(),
                    file,
                    d.path().to_path_buf(),
                    state,
                    devstack_proc::Options::default(),
                    rt.handle().clone(),
                )
            } else {
                Supervisor::new_lane(
                    stack(),
                    file,
                    d.path().to_path_buf(),
                    state.join(format!("lane-{lane}")),
                    devstack_proc::Options::default(),
                    rt.handle().clone(),
                    lane.into(),
                    100,
                    Arc::new(Mutex::new(None)),
                    None,
                )
            }
            .unwrap();
            let ctx = CommandContext {
                repo: d.path().to_path_buf(),
                urls: BTreeMap::new(),
                published: BTreeMap::new(),
                projects: sup.compose_projects(),
                lane: lane.into(),
            };
            assert_eq!(
                expand(
                    "docker compose -p {compose.app.project} exec -T db true",
                    &ctx,
                    &|_| None
                )
                .unwrap(),
                format!("docker compose -p {project} exec -T db true"),
                "lane `{lane}`"
            );
        }
    }
}
