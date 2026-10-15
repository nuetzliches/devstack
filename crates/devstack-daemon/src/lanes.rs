//! Lanes (plan §3.5): another working tree of the same repository (a
//! `git worktree`, an agent's checkout) runs the stack's services beside the
//! main working tree, inside the same daemon.
//!
//! A lane is a [`Supervisor`] of its own: its source root is the worktree, its
//! state lives in `.devstack/lane-<name>/` (snapshot, artifacts, logs), and
//! every service port is moved by `port_step` times the lane's number, so a
//! lane's proxies, candidates and dev servers do not meet the main lane's.
//! The infrastructure is the main lane's: a lane reads its published ports,
//! never starts or stops it, and runs without workers while it does not own the
//! main slot (ADR-0012 interim rule). Per-lane resources on that shared
//! infrastructure (a database per lane) come from `{lane}` / `{lane_suffix}`.
//!
//! The control API of a lane is the same as the main one, under
//! `/v1/lanes/<name>/v1/...`.

use crate::api::{self, AppState};
use crate::supervisor::Supervisor;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A lane as it is remembered across daemon restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneEntry {
    pub name: String,
    /// The lane's working tree (its repository root).
    pub path: PathBuf,
    /// 1, 2, ...: the lane's ports are the stack's plus `number * port_step`.
    pub number: u16,
}

/// A lane as the API shows it.
#[derive(Debug, Clone, Serialize)]
pub struct LaneView {
    pub name: String,
    pub path: PathBuf,
    pub number: u16,
    /// What its ports are moved by.
    pub port_offset: u16,
    /// Its instances serve the main slot (`devstack promote --lane`).
    pub owns_main: bool,
    /// Services that run (healthy or starting), of all.
    pub running: usize,
    pub services: usize,
}

pub struct Lane {
    pub entry: LaneEntry,
    pub sup: Arc<Supervisor>,
    pub router: axum::Router,
    _watcher: Option<crate::watch::Watcher>,
}

pub struct Lanes {
    root_state: PathBuf,
    main: Arc<Supervisor>,
    proc_opts: devstack_proc::Options,
    rt: tokio::runtime::Handle,
    token: Arc<String>,
    port: u16,
    map: Mutex<BTreeMap<String, Arc<Lane>>>,
    /// Lanes being set up right now, so one name is not taken twice.
    pending: Mutex<BTreeSet<String>>,
}

/// `Feature/Agent 12` -> `feature-agent-12`; `main` is the main working tree's.
pub fn lane_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect()
}

/// The stack as a lane runs it: every service port moved by `offset`.
pub fn lane_stack(
    stack: &devstack_config::Stack,
    offset: u16,
) -> Result<devstack_config::Stack, String> {
    let mut s = stack.clone();
    for (name, svc) in s.services.iter_mut() {
        if let Some(p) = svc.port {
            svc.port = Some(
                p.checked_add(offset)
                    .ok_or_else(|| format!("`{name}`: port {p} + {offset} is beyond 65535"))?,
            );
        }
    }
    Ok(s)
}

fn ports(stack: &devstack_config::Stack) -> Vec<u16> {
    stack.services.values().filter_map(|s| s.port).collect()
}

impl Lanes {
    fn file(&self) -> PathBuf {
        self.root_state.join("lanes.json")
    }

    pub fn state_dir(&self, name: &str) -> PathBuf {
        self.root_state.join(format!("lane-{name}"))
    }

    fn save(&self) {
        let entries: Vec<LaneEntry> = self
            .map
            .lock()
            .unwrap()
            .values()
            .map(|l| l.entry.clone())
            .collect();
        if let Ok(text) = serde_json::to_vec_pretty(&entries) {
            let _ = std::fs::write(self.file(), text);
        }
    }

    /// The lanes known from before (not started: a lane starts with `up`).
    pub fn load(
        root_state: PathBuf,
        main: Arc<Supervisor>,
        proc_opts: devstack_proc::Options,
        rt: tokio::runtime::Handle,
        token: Arc<String>,
        port: u16,
    ) -> Arc<Self> {
        let me = Arc::new(Self {
            root_state,
            main,
            proc_opts,
            rt,
            token,
            port,
            map: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeSet::new()),
        });
        let entries: Vec<LaneEntry> = std::fs::read(me.file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        for e in entries {
            match me.open(e.clone()) {
                Ok(l) => {
                    me.map.lock().unwrap().insert(e.name.clone(), l);
                }
                Err(err) => crate::diag::warn(
                    "lane_lost",
                    serde_json::json!({ "lane": e.name, "reason": err }),
                ),
            }
        }
        me
    }

    fn open(&self, entry: LaneEntry) -> Result<Arc<Lane>, String> {
        let repo = devstack_proc::long_path(&entry.path);
        if !repo.is_dir() {
            return Err(format!("{} is gone", repo.display()));
        }
        // The lane's own stack file when its branch has one (it may have changed
        // it), else the main working tree's.
        let own = repo.join("devstack.toml");
        let file = if own.is_file() {
            own
        } else {
            self.main.stack_file().to_path_buf()
        };
        let stack = devstack_config::load(&file).map_err(|e| e.to_string())?;
        let offset = entry.number * stack.lanes.port_step;
        let stack = lane_stack(&stack, offset)?;
        let state = self.state_dir(&entry.name);
        std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
        let sup = Supervisor::new_lane(
            stack,
            file,
            repo,
            state,
            self.proc_opts.clone(),
            self.rt.clone(),
            entry.name.clone(),
        )?;
        sup.refresh_infra();
        let usage = crate::usage::Sampler::start(sup.clone());
        let router = api::router(AppState {
            sup: sup.clone(),
            runs: Arc::new(crate::commands::Runs::default()),
            usage,
            token: self.token.clone(),
            port: self.port,
            // `shutdown` inside a lane stops nothing; `down` stops the lane.
            shutdown: Arc::new(tokio::sync::Notify::new()),
            lanes: None,
        });
        let watcher = crate::watch::start(sup.clone());
        Ok(Arc::new(Lane {
            entry,
            sup,
            router,
            _watcher: watcher,
        }))
    }

    pub fn get(&self, name: &str) -> Option<Arc<Lane>> {
        self.map.lock().unwrap().get(name).cloned()
    }

    /// The lane whose working tree is `path`.
    pub fn by_path(&self, path: &Path) -> Option<Arc<Lane>> {
        let want = devstack_proc::long_path(path);
        self.map
            .lock()
            .unwrap()
            .values()
            .find(|l| devstack_proc::long_path(&l.entry.path) == want)
            .cloned()
    }

    pub fn list(&self) -> Vec<LaneView> {
        let main_running = |s: &Supervisor| {
            let v = s.view();
            let n = v.services.iter().filter(|s| s.status.is_active()).count();
            (n, v.services.len())
        };
        self.map
            .lock()
            .unwrap()
            .values()
            .map(|l| {
                let (running, services) = main_running(&l.sup);
                LaneView {
                    name: l.entry.name.clone(),
                    path: l.entry.path.clone(),
                    number: l.entry.number,
                    port_offset: l.entry.number * l.sup.stack.lanes.port_step,
                    owns_main: l.sup.owns_main(),
                    running,
                    services,
                }
            })
            .collect()
    }

    /// Register a working tree as a lane (its name from the directory unless
    /// given) and set it up; `up` starts it.
    pub fn add(&self, path: &Path, name: Option<&str>) -> Result<Arc<Lane>, String> {
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        if devstack_proc::long_path(&path) == devstack_proc::long_path(&self.main.repo) {
            return Err("that is the main working tree; it is the lane `main` already".into());
        }
        if let Some(l) = self.by_path(&path) {
            return Ok(l);
        }
        let name = lane_name(
            name.unwrap_or_else(|| path.file_name().and_then(|n| n.to_str()).unwrap_or("lane")),
        );
        if name.is_empty() || name == "main" {
            return Err(format!("`{name}` cannot name a lane"));
        }
        if !self.pending.lock().unwrap().insert(name.clone()) {
            return Err(format!("lane `{name}` is being set up"));
        }
        let result = (|| {
            if self.get(&name).is_some() {
                return Err(format!("a lane `{name}` exists for another working tree"));
            }
            // The first number whose ports are taken by nobody: the main lane,
            // another lane, or a foreign process.
            let step = self.main.stack.lanes.port_step;
            let mut taken: BTreeSet<u16> = ports(&self.main.stack).into_iter().collect();
            let used: BTreeSet<u16> = self
                .map
                .lock()
                .unwrap()
                .values()
                .map(|l| l.entry.number)
                .collect();
            for l in self.map.lock().unwrap().values() {
                taken.extend(ports(&l.sup.stack));
            }
            let base = ports(&self.main.stack);
            let number = (1..200u16)
                .filter(|n| !used.contains(n))
                .find(|n| {
                    base.iter().all(|p| {
                        p.checked_add(n * step).is_some_and(|q| {
                            !taken.contains(&q) && !crate::supervisor::port_in_use(q)
                        })
                    })
                })
                .ok_or("no free port range for another lane")?;
            let lane = self.open(LaneEntry {
                name: name.clone(),
                path: path.clone(),
                number,
            })?;
            self.map.lock().unwrap().insert(name.clone(), lane.clone());
            self.save();
            crate::diag::info(
                "lane_added",
                serde_json::json!({ "lane": name, "path": path, "number": number }),
            );
            Ok(lane)
        })();
        self.pending.lock().unwrap().remove(&name);
        result
    }

    /// Stop a lane's services and forget it, its state (snapshot, artifacts,
    /// logs) included. The working tree is not touched.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let lane = self.get(name).ok_or_else(|| format!("no lane `{name}`"))?;
        if lane.sup.owns_main() {
            return Err(format!(
                "lane `{name}` serves the main slot: `devstack promote --lane main` first"
            ));
        }
        lane.sup.down(false);
        self.map.lock().unwrap().remove(name);
        self.save();
        drop(lane);
        let _ = std::fs::remove_dir_all(self.state_dir(name));
        crate::diag::info("lane_removed", serde_json::json!({ "lane": name }));
        Ok(())
    }

    /// `promote --lane`: the lane `name` serves the main ports (`main`: the main
    /// working tree again). The fixed port of every proxied service forwards
    /// to the lane's port, so candidates in the lane switch on it as they do
    /// on their own port; dev servers keep their port in the main lane.
    /// Workers move with the main slot (ADR-0008): the old owner's worker
    /// services restart without them, the new owner's with them.
    pub fn take_main(&self, main: &Arc<Supervisor>, name: &str) -> Result<(), String> {
        let current = main.served_by();
        let target = (name != "main").then(|| name.to_string());
        if current == target {
            return Ok(());
        }
        let lane = match &target {
            Some(n) => {
                let l = self.get(n).ok_or_else(|| format!("no lane `{n}`"))?;
                let down: Vec<String> = main
                    .proxied_services()
                    .into_iter()
                    .filter(|s| {
                        !l.sup
                            .view()
                            .services
                            .iter()
                            .any(|v| &v.name == s && v.status.is_active())
                    })
                    .collect();
                if !down.is_empty() {
                    return Err(format!(
                        "lane `{n}` does not run {}: `devstack up` in its working tree first",
                        down.join(", ")
                    ));
                }
                Some(l)
            }
            None => None,
        };
        // 1. The current owner lets go of the workers.
        match current.as_deref().and_then(|c| self.get(c)) {
            Some(prev) => {
                prev.sup.set_owns_main(false);
                for s in prev.sup.running_worker_services() {
                    let _ = prev.sup.relaunch(&s);
                }
            }
            None => {
                main.set_owns_main(false);
                for s in main.proxied_services() {
                    let _ = main.stop(&s);
                }
                for s in main.running_worker_services() {
                    let _ = main.relaunch(&s);
                }
            }
        }
        // 2. The fixed ports, and the workers, go to the new owner.
        match &lane {
            Some(l) => {
                main.set_served_by(Some(l.entry.name.clone()));
                let views = l.sup.view().services;
                for s in main.proxied_services() {
                    let v = views.iter().find(|v| v.name == s);
                    if let Some(port) = v.and_then(|v| v.port) {
                        main.forward(
                            &s,
                            Some((&l.entry.name, port, v.and_then(|v| v.build_id.clone()))),
                        )?;
                    }
                }
                l.sup.set_owns_main(true);
                for s in l.sup.running_worker_services() {
                    let _ = l.sup.relaunch(&s);
                }
            }
            None => {
                main.set_served_by(None);
                main.set_owns_main(true);
                for s in main.proxied_services() {
                    main.forward(&s, None)?;
                }
                for s in main.proxied_services() {
                    let _ = main.start(&s);
                }
                for s in main.running_worker_services() {
                    let _ = main.relaunch(&s);
                }
            }
        }
        crate::diag::info("main_slot", serde_json::json!({ "lane": name }));
        Ok(())
    }

    /// Stop every lane's services (the daemon is stopping).
    pub fn down_all(&self) {
        let lanes: Vec<Arc<Lane>> = self.map.lock().unwrap().values().cloned().collect();
        for l in lanes {
            l.sup.down(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ports() {
        assert_eq!(lane_name("Feature/Agent 12"), "feature-agent-12");
        assert_eq!(lane_name("--x--"), "x");
        let stack = devstack_config::from_str(
            "version = 1\n[stack]\nname = \"t\"\n[services.api]\nrun = \"x\"\nport = 5000\n[services.job]\nkind = \"job\"\nrun = \"y\"\n",
            Path::new("t.toml"),
        )
        .unwrap();
        assert_eq!(stack.lanes.port_step, 100);
        let l = lane_stack(&stack, 200).unwrap();
        assert_eq!(l.services["api"].port, Some(5200));
        assert_eq!(l.services["job"].port, None);
        assert!(lane_stack(&stack, 65000).is_err());
    }
}
