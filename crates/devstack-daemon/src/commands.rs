//! Runs of the stack file's `commands` (plan §4.4, §6): steps in order, every
//! run with an id, its own log and a final status, kept for the history.

use crate::state::{EventBody, now_ms};
use crate::supervisor::Supervisor;
use devstack_config::interpolate::{Placeholder, Resolve, expand};
use devstack_config::model::StepAction;
use serde::Serialize;
use std::collections::BTreeMap;
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
}

/// Placeholders a command may use: repository and service URLs.
struct CommandContext {
    repo: PathBuf,
    urls: BTreeMap<String, String>,
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
            other => Err(format!("`{other}` is not available in a command")),
        }
    }
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
    std::thread::spawn(move || {
        let result = execute(&sup, &cmd.steps, &log);
        let status = if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        };
        runs.update(id, |r| {
            r.finished = Some(now_ms());
            r.status = status.into();
            r.error = result.err();
        });
        sup.emit(EventBody::Command {
            command: name,
            run: id,
            status: status.into(),
        });
    });
    Ok(id)
}

fn execute(
    sup: &Arc<Supervisor>,
    steps: &[devstack_config::model::Step],
    log: &PathBuf,
) -> Result<(), String> {
    let mut urls = BTreeMap::new();
    for (n, s) in &sup.stack.services {
        if let Some(p) = s.port {
            urls.insert(n.clone(), format!("{}://127.0.0.1:{p}", s.scheme()));
        }
    }
    let ctx = CommandContext {
        repo: sup.repo.clone(),
        urls,
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
        let action = step.action().map_err(|_| format!("step {i} is invalid"))?;
        let _ = writeln!(out, "--- step {} : {action:?}", i + 1);
        let r = match action {
            StepAction::Exec { command, cwd } => {
                let command = expand(command, &ctx, &env).map_err(|e| e.to_string())?;
                let cwd = match cwd {
                    Some(c) => sup
                        .repo
                        .join(expand(c, &ctx, &env).map_err(|e| e.to_string())?),
                    None => sup.repo.clone(),
                };
                let file = out.try_clone().map_err(|e| e.to_string())?;
                let spec = devstack_proc::Spec {
                    name: "command".into(),
                    command,
                    cwd,
                    env: BTreeMap::new(),
                    log: file,
                };
                let (mut tree, _) =
                    devstack_proc::spawn(spec, sup.proc_options()).map_err(|e| e.to_string())?;
                let code = loop {
                    if let Some(c) = tree.try_exit_code() {
                        break c;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                if code == 0 {
                    Ok(())
                } else {
                    Err(format!("step {} exited with code {code}", i + 1))
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
            StepAction::Run(s) => sup.stop(s).and_then(|_| sup.start(s)),
        };
        if let Err(e) = r {
            let _ = writeln!(out, "--- failed: {e}");
            return Err(e);
        }
    }
    let _ = writeln!(out, "--- done");
    Ok(())
}
