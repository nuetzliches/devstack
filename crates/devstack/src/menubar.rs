//! The optional macOS menu is a client of the existing CLI and daemon API
//! (ADR-0060). AppKit stays on the main thread; this worker owns all I/O.

pub(crate) fn run() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return controller::run();

    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("the menu bar client is available on macOS only")
}

#[cfg(any(target_os = "macos", test))]
mod controller {
    #[cfg(target_os = "macos")]
    use anyhow::anyhow;
    use anyhow::{Context, Result, bail};
    use devstack_daemon::registry;
    use devstack_desktop::menubar::{Entry, Menu};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const REFRESH: Duration = Duration::from_secs(5);
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Hub {
        Checking,
        Running,
        Stopped,
        Foreign,
    }

    #[derive(Clone, Debug)]
    struct Stack {
        entry: registry::Entry,
        daemon: bool,
        status: String,
        details: Vec<String>,
    }

    struct Snapshot {
        hub: Hub,
        stacks: Vec<Stack>,
        message: Option<String>,
        busy: bool,
    }

    impl Default for Snapshot {
        fn default() -> Self {
            Self {
                hub: Hub::Checking,
                stacks: Vec::new(),
                message: None,
                busy: false,
            }
        }
    }

    fn action(label: &str, id: &str, enabled: bool) -> Entry {
        Entry::Action {
            label: label.into(),
            id: id.into(),
            enabled,
        }
    }

    fn menu(s: &Snapshot) -> Menu {
        let hub = match s.hub {
            Hub::Checking => "Checking Dashboard Hub…",
            Hub::Running => "● Dashboard Hub running",
            Hub::Stopped => "○ Dashboard Hub stopped",
            Hub::Foreign => "⚠ Dashboard Hub port occupied",
        };
        let available = !s.busy;
        let mut entries = vec![
            Entry::Label(hub.into()),
            action(
                "Open Dashboard",
                "dashboard",
                available && matches!(s.hub, Hub::Running | Hub::Stopped),
            ),
        ];
        if let Some(message) = &s.message {
            entries.push(Entry::Label(short(message, 180)));
        }
        entries.push(Entry::Separator);
        match s.hub {
            Hub::Running => entries.push(action(
                "Stop Dashboard Hub (keep stacks running)",
                "hub-stop",
                available,
            )),
            Hub::Stopped => entries.push(action("Start Dashboard Hub", "hub-start", available)),
            Hub::Foreign => entries.push(Entry::Label(format!(
                "Another process uses port {}",
                devstack_hub::port()
            ))),
            Hub::Checking => {}
        }
        entries.push(Entry::Separator);
        entries.push(Entry::Label("Local stacks".into()));
        if s.stacks.is_empty() {
            entries.push(Entry::Label(if s.hub == Hub::Checking {
                "Checking registered stacks…".into()
            } else {
                "No registered stacks — run devstack up in a repository".into()
            }));
        }
        for stack in &s.stacks {
            let id = &stack.entry.id;
            let mut children = vec![Entry::Label(short(&stack.status, 120))];
            children.extend(stack.details.iter().map(|d| Entry::Label(short(d, 150))));
            children.push(Entry::Separator);
            children.push(action(
                "Open Dashboard",
                &format!("dashboard:{id}"),
                available && s.hub != Hub::Foreign && stack.entry.file.is_file(),
            ));
            children.push(action(
                "Start Stack",
                &format!("start:{id}"),
                available && stack.entry.file.is_file(),
            ));
            children.push(action(
                "Stop Main Stack & Containers",
                &format!("stop:{id}"),
                available && stack.daemon && stack.entry.file.is_file(),
            ));
            children.push(Entry::Label(
                "Running lanes keep their services and containers".into(),
            ));
            children.push(Entry::Separator);
            children.push(action(
                "Open Folder",
                &format!("folder:{id}"),
                available && stack.entry.repo.is_dir(),
            ));
            entries.push(Entry::Submenu {
                label: format!(
                    "{} — {}",
                    short(&stack.entry.name, 60),
                    short(&stack.status, 50)
                ),
                entries: children,
            });
        }
        entries.extend([
            Entry::Separator,
            action("Refresh Status", "refresh", available),
            action("Give Feedback…", "feedback", available),
            Entry::Label(format!("devstack {}", devstack_config::VERSION)),
            action("Quit Menu Bar (keep stacks running)", "quit", true),
        ]);
        Menu {
            tooltip: format!("devstack — {hub}"),
            entries,
        }
    }

    fn short(text: &str, max: usize) -> String {
        let clean: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        if clean.chars().count() > max {
            format!(
                "{}…",
                clean
                    .chars()
                    .take(max.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            clean
        }
    }

    /// The API keeps intentionally skipped containers managed (ADR-0058),
    /// without exposing their startup policy. Only use the local profile when
    /// the daemon reports that its configuration still matches that file.
    fn profile_skips(view: &Value, file: Option<&Path>) -> Option<BTreeSet<(String, String)>> {
        let Some(profile) = view["profile"].as_str() else {
            return Some(BTreeSet::new());
        };
        if view["config_changed"] == true {
            return None;
        }
        let config = devstack_config::load(file?).ok()?;
        config
            .with_profile(profile)
            .ok()?
            .profile
            .map(|active| active.skip)
    }

    /// Include Compose alongside native services, without calling a daemon
    /// healthy merely because it answers. Missing infrastructure stays unknown.
    fn stack_status(view: &Value, file: Option<&Path>) -> (String, Vec<String>) {
        let services: Vec<&Value> = view["services"].as_array().into_iter().flatten().collect();
        let infra: Vec<&Value> = view["infra"].as_array().into_iter().flatten().collect();
        let skips = profile_skips(view, file);
        let mut left_out = 0;
        let containers: Vec<&Value> = infra
            .iter()
            .flat_map(|g| {
                g["containers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(move |c| (g, c))
            })
            .filter(|(_, c)| c["managed"] != false)
            .filter_map(|(g, c)| {
                let stopped_normally = matches!(c["state"].as_str(), Some("stopped" | "created"))
                    || (c["state"] == "exited"
                        && c["exit_code"]
                            .as_i64()
                            .is_some_and(|code| matches!(code, 0 | 130 | 137 | 143)));
                let intentionally_stopped = stopped_normally
                    && skips.as_ref().is_some_and(|skipped| {
                        skipped.contains(&(
                            g["group"].as_str().unwrap_or("").to_string(),
                            c["service"].as_str().unwrap_or("").to_string(),
                        ))
                    });
                if intentionally_stopped {
                    left_out += 1;
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        let native_running = services
            .iter()
            .filter(|s| matches!(s["status"]["state"].as_str(), Some("running" | "healthy")))
            .count();
        let containers_running = containers
            .iter()
            .filter(|c| c["state"] == "running")
            .count();
        let issue = infra
            .iter()
            .find_map(|g| g["error"].as_str())
            .map(str::to_string)
            .or_else(|| {
                services
                    .iter()
                    .find_map(|s| match s["status"]["state"].as_str() {
                        Some("failed" | "unhealthy") => Some(format!(
                            "{}: {}",
                            s["name"].as_str().unwrap_or("service"),
                            s["status"]["reason"].as_str().unwrap_or("needs attention")
                        )),
                        Some("exited" | "completed")
                            if s["status"]["exit_code"].as_i64().is_some_and(|c| c != 0) =>
                        {
                            Some(format!(
                                "{}: exited with {}",
                                s["name"].as_str().unwrap_or("service"),
                                s["status"]["exit_code"]
                            ))
                        }
                        _ => None,
                    })
            })
            .or_else(|| {
                services
                    .iter()
                    .find_map(|s| match s["candidate"]["state"].as_str() {
                        Some("failed" | "held") => Some(format!(
                            "{}: candidate {} — {}",
                            s["name"].as_str().unwrap_or("service"),
                            s["candidate"]["state"].as_str().unwrap_or(""),
                            s["candidate"]["reason"]
                                .as_str()
                                .unwrap_or("open Dashboard for details")
                        )),
                        _ => None,
                    })
            })
            .or_else(|| {
                services.iter().find_map(|s| {
                    (s["rebuild"]["state"] == "failed").then(|| {
                        let error = s["rebuild"]["errors"]
                            .as_array()
                            .and_then(|lines| {
                                lines
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .find(|line| !line.trim().is_empty())
                            })
                            .unwrap_or("open Dashboard for details");
                        format!(
                            "{}: rebuild failed — {error}",
                            s["name"].as_str().unwrap_or("service")
                        )
                    })
                })
            })
            .or_else(|| {
                containers
                    .iter()
                    .find(|c| {
                        c["health"] == "unhealthy"
                            || c["state"] == "dead"
                            || (c["state"] == "exited"
                                && c["exit_code"]
                                    .as_i64()
                                    .is_some_and(|n| !matches!(n, 0 | 130 | 137 | 143)))
                    })
                    .map(|c| {
                        format!(
                            "{}: needs attention",
                            c["service"].as_str().unwrap_or("container")
                        )
                    })
            });
        let starting = services
            .iter()
            .any(|s| matches!(s["status"]["state"].as_str(), Some("building" | "starting")))
            || containers.iter().any(|c| {
                c["health"] == "starting"
                    || matches!(c["state"].as_str(), Some("created" | "restarting"))
            });
        let running = native_running + containers_running;
        let total = services.len() + containers.len();
        let completed = services
            .iter()
            .filter(|s| s["status"]["state"] == "completed" && s["status"]["exit_code"] == 0)
            .count();
        let expected_pending = services.iter().any(|s| {
            s["autostart"] != false
                && !matches!(s["status"]["state"].as_str(), Some("running" | "healthy"))
                && !(s["status"]["state"] == "completed" && s["status"]["exit_code"] == 0)
        });
        let status = if issue.is_some() {
            "Needs attention"
        } else if starting {
            "Starting"
        } else if skips.is_none() && containers_running != containers.len() {
            "Status incomplete"
        } else if total == 0 {
            "Daemon running"
        } else if running > 0 && !expected_pending && containers_running == containers.len() {
            "Running"
        } else if running > 0 {
            "Partly running"
        } else if completed > 0 && !expected_pending && containers.is_empty() {
            "Jobs completed"
        } else {
            "Services stopped"
        };
        let mut details = vec![
            format!(
                "Native services: {native_running}/{} running",
                services.len()
            ),
            format!(
                "Compose containers: {containers_running}/{} running",
                containers.len()
            ),
        ];
        if completed > 0 {
            details.push(format!("Jobs completed successfully: {completed}"));
        }
        if left_out > 0 {
            details.push(format!("Profile leaves out {left_out} stopped containers"));
        }
        if skips.is_none() {
            details.push(
                "Profile container expectations unavailable; open Dashboard for details".into(),
            );
        }
        if let Some(issue) = issue {
            details.push(issue);
        }
        (status.into(), details)
    }

    fn collect_stack(entry: registry::Entry, stop: &AtomicBool) -> Stack {
        let mut stack = Stack {
            entry,
            daemon: false,
            status: "Daemon stopped".into(),
            details: vec!["Containers are not checked while the daemon is stopped".into()],
        };
        let Some(parent) = stack.entry.file.parent() else {
            return stack;
        };
        let Some(info) = devstack_daemon::read_info(&parent.join(".devstack")) else {
            return stack;
        };
        if !devstack_proc::pid_alive(info.pid) {
            return stack;
        }
        stack.daemon = true;
        stack.status = "Checking status".into();
        stack.details.clear();
        if info.stopping {
            stack.status = "Stopping".into();
            return stack;
        }
        if stop.load(Ordering::Acquire) {
            return stack;
        }
        let result = (|| -> Result<Value> {
            let token = devstack_daemon::token::load().context("user token is missing")?;
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(REQUEST_TIMEOUT))
                .http_status_as_error(false)
                .build()
                .new_agent();
            let mut response = agent
                .get(&format!("http://127.0.0.1:{}/v1/stack", info.port))
                .header("Authorization", &format!("Bearer {token}"))
                .call()?;
            if response.status() != 200 {
                bail!("daemon returned HTTP {}", response.status());
            }
            Ok(serde_json::from_str(
                &response.body_mut().read_to_string()?,
            )?)
        })();
        match result {
            Ok(view) => {
                (stack.status, stack.details) = stack_status(&view, Some(&stack.entry.file))
            }
            Err(e) => {
                stack.status = "Status unavailable".into();
                stack.details.push(short(&format!("{e:#}"), 150));
            }
        }
        stack
    }

    fn refresh(s: &mut Snapshot, stop: &AtomicBool) {
        if stop.load(Ordering::Acquire) {
            return;
        }
        s.hub = match devstack_hub::who_answers(devstack_hub::port()) {
            devstack_hub::Listener::Ours => Hub::Running,
            devstack_hub::Listener::Nothing => Hub::Stopped,
            devstack_hub::Listener::Foreign => Hub::Foreign,
        };
        let mut stacks = Vec::new();
        for entry in registry::list() {
            if stop.load(Ordering::Acquire) {
                return;
            }
            stacks.push(collect_stack(entry, stop));
        }
        s.stacks = stacks;
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Invocation {
        /// `None` means this executable; folders and feedback use macOS's `open`.
        program: Option<PathBuf>,
        args: Vec<OsString>,
        directory: Option<PathBuf>,
        progress: String,
    }

    fn invocation(id: &str, s: &Snapshot) -> Option<Invocation> {
        let args = |words: &[&str]| words.iter().map(OsString::from).collect();
        let simple = |words: &[&str], progress: &str| Invocation {
            program: None,
            args: args(words),
            directory: None,
            progress: progress.into(),
        };
        match id {
            "feedback" => {
                return Some(Invocation {
                    program: Some("/usr/bin/open".into()),
                    args: args(&[
                        "--",
                        "https://github.com/nuetzliches/devstack/issues/new/choose",
                    ]),
                    directory: None,
                    progress: "Opening GitHub feedback…".into(),
                });
            }
            // `/` is a directory, so `ui` opens the hub without inheriting a
            // stack from the menu client's launch directory.
            "dashboard" if s.hub != Hub::Foreign => {
                return Some(simple(&["--file", "/", "ui"], "Opening Dashboard…"));
            }
            "hub-start" if s.hub == Hub::Stopped => {
                return Some(simple(&["hub", "start"], "Starting Dashboard Hub…"));
            }
            "hub-stop" if s.hub == Hub::Running => {
                return Some(simple(&["hub", "stop"], "Stopping Dashboard Hub…"));
            }
            _ => {}
        }
        let (kind, id) = id.split_once(':')?;
        let stack = s.stacks.iter().find(|stack| stack.entry.id == id)?;
        let entry = &stack.entry;
        let mut command = Invocation {
            program: None,
            args: vec!["--file".into(), entry.file.as_os_str().into()],
            directory: Some(entry.repo.clone()),
            progress: String::new(),
        };
        match kind {
            "dashboard" if entry.file.is_file() && s.hub != Hub::Foreign => {
                command.args.push("ui".into());
                command.progress = format!("Opening {} Dashboard…", entry.name);
            }
            "start" if entry.file.is_file() => {
                command.args.push("up".into());
                command.progress = format!("Starting {}…", entry.name);
            }
            "stop" if stack.daemon && entry.file.is_file() => {
                command.args.extend(args(&["down", "--infra"]));
                command.progress = format!("Stopping {} main stack and containers…", entry.name);
            }
            "folder" if entry.repo.is_dir() => {
                command.program = Some("/usr/bin/open".into());
                command.args = vec!["--".into(), entry.repo.as_os_str().into()];
                command.progress = format!("Opening {} folder…", entry.name);
            }
            _ => return None,
        }
        Some(command)
    }

    struct OutputLog {
        file: File,
        path: PathBuf,
    }

    impl OutputLog {
        fn new() -> Result<Self> {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let path = std::env::temp_dir()
                .join(format!("devstack-menubar-{}-{now}.log", std::process::id()));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            Ok(Self {
                file: options.open(&path)?,
                path,
            })
        }

        fn tail(&mut self) -> String {
            let _ = self
                .file
                .seek(SeekFrom::End(-4096))
                .or_else(|_| self.file.seek(SeekFrom::Start(0)));
            let mut bytes = Vec::new();
            let _ = self.file.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).trim().to_string()
        }
    }

    impl Drop for OutputLog {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn execute(call: &Invocation, stop: &AtomicBool) -> Result<()> {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut log = OutputLog::new()?;
        let program = call.program.clone().map(Ok).unwrap_or_else(|| {
            std::env::current_exe().map(|exe| devstack_desktop::stable_exe(&exe))
        })?;
        let mut command = Command::new(program);
        command
            .args(&call.args)
            .stdin(Stdio::null())
            .stdout(log.file.try_clone()?)
            .stderr(log.file.try_clone()?);
        if let Some(dir) = &call.directory {
            command.current_dir(dir);
        }
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut child = command.spawn().context("could not start command")?;
        let started = Instant::now();
        loop {
            if stop.load(Ordering::Acquire) {
                // Only the client command belongs to this menu process. The
                // existing daemon and hub keep their own lifetime.
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
            match child.try_wait()? {
                Some(status) if status.success() => return Ok(()),
                Some(status) => {
                    let output = log.tail();
                    bail!(
                        "command failed ({status}){}",
                        if output.is_empty() {
                            String::new()
                        } else {
                            format!(": {output}")
                        }
                    );
                }
                None if started.elapsed() >= Duration::from_secs(1800) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("command timed out; inspect the Dashboard for its current state");
                }
                None => std::thread::sleep(Duration::from_millis(80)),
            }
        }
    }

    fn worker(actions: Receiver<String>, updates: Sender<Menu>, stop: Arc<AtomicBool>) {
        let mut s = Snapshot::default();
        loop {
            refresh(&mut s, &stop);
            if stop.load(Ordering::Acquire) || updates.send(menu(&s)).is_err() {
                return;
            }
            let id = match actions.recv_timeout(REFRESH) {
                Ok(id) if id == "quit" => return,
                Ok(id) => id,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            if id == "refresh" {
                continue;
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            let Some(call) = invocation(&id, &s) else {
                continue;
            };
            s.busy = true;
            s.message = Some(call.progress.clone());
            if updates.send(menu(&s)).is_err() {
                return;
            }
            let result = execute(&call, &stop);
            if stop.load(Ordering::Acquire) {
                return;
            }
            s.busy = false;
            s.message = Some(match result {
                Ok(()) => "Command completed — status refreshes automatically".into(),
                Err(e) => {
                    eprintln!("menu bar: {e:#}");
                    let detail = format!("{e:#}");
                    format!(
                        "⚠ {}",
                        short(detail.lines().next_back().unwrap_or(&detail), 170)
                    )
                }
            });
            // A click queued just before a busy snapshot must not repeat an
            // up/down after that command finishes. Quit still takes priority.
            for queued in actions.try_iter() {
                if queued == "quit" {
                    return;
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn single_instance() -> Result<File> {
        let dir = devstack_daemon::token::token_path()
            .parent()
            .context("token directory")?
            .to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("menubar.lock"))?;
        // The descriptor holds the lock until this process exits. No PID file
        // can become stale or refer to a recycled process.
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!("a devstack menu bar client is already running for this user")
            }
            Err(e) => bail!("could not lock the menu bar client: {e}"),
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn run() -> Result<()> {
        let _instance = single_instance()?;
        let (actions_tx, actions_rx) = mpsc::channel();
        let (updates_tx, updates_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let shutdown = actions_tx.clone();
        let worker = std::thread::Builder::new()
            .name("devstack-menu".into())
            .spawn(move || worker(actions_rx, updates_tx, worker_stop))?;
        let result =
            devstack_desktop::menubar::run(menu(&Snapshot::default()), updates_rx, actions_tx);
        stop.store(true, Ordering::Release);
        let _ = shutdown.send("quit".into());
        worker
            .join()
            .map_err(|_| anyhow!("menu bar worker panicked"))?;
        result.map_err(|e| anyhow!(e))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        fn stack_status(view: &Value) -> (String, Vec<String>) {
            super::stack_status(view, None)
        }

        fn find<'a>(entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
            entries.iter().find_map(|entry| match entry {
                Entry::Action { id: found, .. } if found == id => Some(entry),
                Entry::Submenu { entries, .. } => find(entries, id),
                _ => None,
            })
        }

        fn enabled(menu: &Menu, id: &str) -> bool {
            matches!(
                find(&menu.entries, id),
                Some(Entry::Action { enabled: true, .. })
            )
        }

        fn fixture() -> (tempfile::TempDir, Snapshot) {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("a file; $(never execute).toml");
            std::fs::write(&file, "").unwrap();
            let stack = Stack {
                entry: registry::Entry {
                    id: "ab12".into(),
                    name: "Example stack".into(),
                    file,
                    repo: temp.path().to_path_buf(),
                },
                daemon: true,
                status: "Running".into(),
                details: vec![],
            };
            (
                temp,
                Snapshot {
                    hub: Hub::Running,
                    stacks: vec![stack],
                    message: None,
                    busy: false,
                },
            )
        }

        #[test]
        fn a_foreign_hub_cannot_be_started_stopped_or_sent_dashboard_credentials() {
            let (_temp, mut s) = fixture();
            s.hub = Hub::Foreign;
            let menu = menu(&s);
            assert!(!enabled(&menu, "dashboard"));
            assert!(!enabled(&menu, "dashboard:ab12"));
            assert!(find(&menu.entries, "hub-start").is_none());
            assert!(find(&menu.entries, "hub-stop").is_none());
            assert!(invocation("dashboard", &s).is_none());
            assert!(invocation("hub-stop", &s).is_none());
        }

        #[test]
        fn busy_menu_disables_actions_but_quit_still_works() {
            let (_temp, mut s) = fixture();
            s.busy = true;
            let menu = menu(&s);
            for id in [
                "dashboard",
                "hub-stop",
                "start:ab12",
                "stop:ab12",
                "folder:ab12",
                "refresh",
            ] {
                assert!(!enabled(&menu, id), "{id}");
            }
            assert!(enabled(&menu, "quit"));
        }

        #[test]
        fn offline_registered_stacks_can_start_and_keep_their_folder() {
            let (_temp, mut s) = fixture();
            s.hub = Hub::Stopped;
            s.stacks[0].daemon = false;
            let menu = menu(&s);
            assert!(enabled(&menu, "hub-start"));
            assert!(enabled(&menu, "dashboard"));
            assert!(enabled(&menu, "start:ab12"));
            assert!(enabled(&menu, "folder:ab12"));
            assert!(!enabled(&menu, "stop:ab12"));
        }

        #[test]
        fn lifecycle_and_folder_paths_are_separate_arguments_and_lanes_are_preserved() {
            let (_temp, s) = fixture();
            let call = invocation("stop:ab12", &s).unwrap();
            assert_eq!(
                call.args,
                vec![
                    OsString::from("--file"),
                    s.stacks[0].entry.file.as_os_str().into(),
                    "down".into(),
                    "--infra".into()
                ]
            );
            assert_eq!(call.program, None);
            let folder = invocation("folder:ab12", &s).unwrap();
            assert_eq!(
                folder.args,
                vec![
                    OsString::from("--"),
                    s.stacks[0].entry.repo.as_os_str().into()
                ]
            );
            assert_eq!(folder.program, Some("/usr/bin/open".into()));
            assert!(invocation("start:unknown", &s).is_none());
        }

        #[test]
        fn feedback_opens_only_the_official_issue_chooser_without_submitting_data() {
            let (_temp, mut s) = fixture();
            s.hub = Hub::Foreign;
            assert!(enabled(&menu(&s), "feedback"));
            let call = invocation("feedback", &s).unwrap();
            assert_eq!(call.program, Some("/usr/bin/open".into()));
            assert_eq!(
                call.args,
                vec![
                    OsString::from("--"),
                    OsString::from("https://github.com/nuetzliches/devstack/issues/new/choose")
                ]
            );
            assert_eq!(call.directory, None);
            assert!(invocation("feedback:https://example.invalid", &s).is_none());
        }

        #[test]
        fn compose_is_counted_and_an_unhealthy_container_is_not_reported_running() {
            let (state, details) = stack_status(&json!({"services": [], "infra": [{"containers": [
                {"service": "db", "state": "running", "health": "unhealthy", "managed": true}
            ]}]}));
            assert_eq!(state, "Needs attention");
            assert!(
                details
                    .iter()
                    .any(|d| d == "Compose containers: 1/1 running")
            );
            let (state, _) = stack_status(&json!({"services": [], "infra": [{"containers": [
                {"state": "running", "health": "healthy", "managed": true}
            ]}]}));
            assert_eq!(state, "Running");
        }

        #[test]
        fn daemon_liveness_and_completed_jobs_do_not_claim_stack_health() {
            assert_eq!(
                stack_status(&json!({"services": [], "infra": []})).0,
                "Daemon running"
            );
            assert_eq!(
                stack_status(
                    &json!({"services": [{"status": {"state": "starting"}}], "infra": []})
                )
                .0,
                "Starting"
            );
            assert_eq!(stack_status(&json!({"services": [{"name": "api", "status": {"state": "failed", "reason": "build failed"}}], "infra": []})).0, "Needs attention");
            assert_eq!(stack_status(&json!({"services": [], "infra": [{"error": "engine unavailable", "containers": []}]})).0, "Needs attention");
        }

        #[test]
        fn successful_jobs_and_stopped_optional_tools_do_not_make_a_running_stack_partial() {
            let (state, details) = stack_status(&json!({"services": [
                {"name": "api", "autostart": true, "status": {"state": "running"}},
                {"name": "prepare", "autostart": true, "status": {"state": "completed", "exit_code": 0}},
                {"name": "tool", "autostart": false, "status": {"state": "stopped"}}
            ], "infra": []}));
            assert_eq!(state, "Running");
            assert!(details.contains(&"Jobs completed successfully: 1".to_string()));
        }

        fn profile_fixture() -> (tempfile::TempDir, PathBuf, Value) {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("devstack.toml");
            std::fs::write(
                &file,
                r#"
version = 1
[stack]
name = "example"
[services.api]
run = "example-api"
[services.worker]
run = "example-worker"
[compose.app]
project = "example-app"
files = ["compose.yml"]
services = ["db", "cache"]
[compose.search]
project = "example-search"
files = ["compose.yml"]
services = ["engine"]
[profiles.lite]
skip = ["compose.app.cache", "compose.search", "worker"]
"#,
            )
            .unwrap();
            let view = json!({
                "profile": "lite", "config_changed": false,
                "services": [
                    {"name": "api", "autostart": true, "status": {"state": "running"}},
                    {"name": "worker", "autostart": false, "status": {"state": "stopped"}}
                ],
                "infra": [
                    {"group": "app", "containers": [
                        {"service": "db", "state": "running", "health": "healthy", "managed": true},
                        {"service": "cache", "state": "exited", "health": "", "exit_code": 0, "managed": true}
                    ]},
                    {"group": "search", "containers": [
                        {"service": "engine", "state": "exited", "health": "", "exit_code": 0, "managed": true}
                    ]}
                ]
            });
            (temp, file, view)
        }

        #[test]
        fn a_profile_can_leave_out_a_container_a_group_and_a_native_service() {
            let (_temp, file, view) = profile_fixture();
            let (status, details) = super::stack_status(&view, Some(&file));
            assert_eq!(status, "Running");
            assert!(details.contains(&"Compose containers: 1/1 running".to_string()));
            assert!(details.contains(&"Profile leaves out 2 stopped containers".to_string()));
        }

        #[test]
        fn a_skipped_container_started_on_demand_is_still_counted_and_checked() {
            let (_temp, file, mut view) = profile_fixture();
            let cache = &mut view["infra"][0]["containers"][1];
            cache["state"] = json!("running");
            cache["health"] = json!("healthy");
            let (status, details) = super::stack_status(&view, Some(&file));
            assert_eq!(status, "Running");
            assert!(details.contains(&"Compose containers: 2/2 running".to_string()));
            view["infra"][0]["containers"][1]["health"] = json!("unhealthy");
            assert_eq!(super::stack_status(&view, Some(&file)).0, "Needs attention");
            view["infra"][0]["containers"][1]["state"] = json!("dead");
            assert_eq!(super::stack_status(&view, Some(&file)).0, "Needs attention");
            view["infra"][0]["containers"][1]["health"] = json!("");
            view["infra"][0]["containers"][1]["state"] = json!("exited");
            view["infra"][0]["containers"][1]["exit_code"] = json!(1);
            assert_eq!(super::stack_status(&view, Some(&file)).0, "Needs attention");
            view["infra"][0]["containers"][1]["exit_code"] = Value::Null;
            assert_eq!(super::stack_status(&view, Some(&file)).0, "Partly running");
            for code in [0, 130, 137, 143] {
                view["infra"][0]["containers"][1]["exit_code"] = json!(code);
                assert_eq!(super::stack_status(&view, Some(&file)).0, "Running");
            }
        }

        #[test]
        fn changed_or_unavailable_profile_config_does_not_hide_stopped_containers() {
            let (_temp, file, mut view) = profile_fixture();
            view["config_changed"] = json!(true);
            let (status, details) = super::stack_status(&view, Some(&file));
            assert_eq!(status, "Status incomplete");
            assert!(details.contains(&"Compose containers: 1/3 running".to_string()));
            assert!(
                details
                    .iter()
                    .any(|detail| detail.starts_with("Profile container expectations unavailable"))
            );
            view["config_changed"] = json!(false);
            std::fs::remove_file(&file).unwrap();
            assert_eq!(
                super::stack_status(&view, Some(&file)).0,
                "Status incomplete"
            );
        }

        #[test]
        fn a_failed_or_held_candidate_needs_attention_while_the_main_service_still_runs() {
            for state in ["failed", "held"] {
                let (status, _) = stack_status(&json!({"services": [
                    {"name": "api", "status": {"state": "healthy"}, "candidate": {"state": state, "reason": "inspect update"}}
                ], "infra": []}));
                assert_eq!(status, "Needs attention");
            }
        }

        #[test]
        fn a_failed_live_rebuild_needs_attention_even_when_its_dev_server_is_healthy() {
            for errors in [
                json!(["", "[ERROR] TS2304: Cannot find name 'missing'."]),
                json!([]),
            ] {
                let (status, details) = stack_status(&json!({"services": [{
                    "name": "web", "kind": "devserver", "autostart": true,
                    "status": {"state": "healthy"},
                    "rebuild": {"state": "failed", "since": 1234, "ms": 120, "tool": "angular", "errors": errors}
                }], "infra": []}));
                assert_eq!(status, "Needs attention");
                assert!(
                    details
                        .iter()
                        .any(|detail| detail.starts_with("web: rebuild failed — "))
                );
                assert!(details.iter().any(|detail| detail.contains("TS2304")
                    || detail.contains("open Dashboard for details")));
            }
        }

        #[test]
        fn an_offline_collector_and_a_cancelled_worker_do_no_network_or_child_work() {
            let (_temp, s) = fixture();
            let stop = Arc::new(AtomicBool::new(true));
            let stack = collect_stack(s.stacks[0].entry.clone(), &stop);
            assert_eq!(stack.status, "Daemon stopped");
            let (_actions, receiver) = mpsc::channel();
            let (sender, updates) = mpsc::channel();
            worker(receiver, sender, stop);
            assert!(updates.try_recv().is_err());
        }

        #[test]
        fn cancellation_does_not_create_a_child_or_modify_a_stack() {
            let cancelled = AtomicBool::new(true);
            execute(
                &Invocation {
                    program: Some("/this/does/not/exist".into()),
                    args: vec![],
                    directory: None,
                    progress: String::new(),
                },
                &cancelled,
            )
            .unwrap();
        }

        #[test]
        fn labels_cannot_inject_menu_lines_and_are_unicode_safe() {
            assert_eq!(short("two\nlines\t", 20), "two lines ");
            assert_eq!(short("🔧🔧🔧", 2), "🔧…");
        }
    }
}
