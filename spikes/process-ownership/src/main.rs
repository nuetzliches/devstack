//! Spike (a): process ownership (plan §7.1, ADR-0009, ADR-0016).
//!
//! `procspike run` starts services the way the daemon will (one Job Object per
//! service on Windows), records which processes belong to them, holds, then
//! stops them gracefully and escalates to a kill. The test harness
//! (`run-tests.mjs`) drives it through the scenarios a1-a8 and checks the
//! outcome from the outside.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod win;

#[derive(Parser)]
#[command(about = "Spike (a): process ownership")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum StopMethod {
    /// CTRL_BREAK to the service's process group (shares our console)
    Break,
    /// Ctrl+C written into a pseudo console per service
    Conpty,
    /// Unix: SIGTERM to the service's process group
    Signal,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start services, record their processes, hold, stop.
    Run {
        #[arg(long, value_enum, default_value = "conpty")]
        method: StopMethod,
        /// Seconds to wait for a graceful stop before killing the job.
        #[arg(long, default_value_t = 20)]
        stop_timeout: u64,
        /// Seconds to run before stopping; 0 = run until killed.
        #[arg(long, default_value_t = 10)]
        hold: u64,
        /// Directory for the lock, logs, pids.json and report.json.
        #[arg(long)]
        state: PathBuf,
        /// Negative control: make the job handle inheritable (ADR-0009 condition 1).
        #[arg(long)]
        inheritable_job: bool,
        /// Negative control: allow breakaway from the job (ADR-0009 condition 2).
        #[arg(long)]
        allow_breakaway: bool,
        /// Allocate a hidden console when started without one (detached daemon).
        #[arg(long)]
        alloc_console: bool,
        /// Unix: put each service in a cgroup under this directory.
        #[arg(long)]
        cgroup_root: Option<PathBuf>,
        /// Unix: put a shim between supervisor and each service.
        #[arg(long)]
        shim: bool,
        /// Services as name=command line.
        #[arg(required = true)]
        services: Vec<String>,
    },
    /// Unix: shim between supervisor and service (internal).
    Shim {
        #[arg(long)]
        ppid: u32,
        #[arg(long)]
        cgroup: Option<PathBuf>,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Unix: kill what a previous instance recorded in <state>/services.json.
    Cleanup {
        #[arg(long)]
        state: PathBuf,
        /// Use this file instead of <state>/services.json (tests).
        #[arg(long)]
        records: Option<PathBuf>,
    },
    /// Fixture: try to start a child outside the current job.
    Breakaway {
        /// File to write the outcome to.
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Serialize)]
struct StopReport {
    service: String,
    graceful_signal_ok: bool,
    signal_error: Option<String>,
    stopped_gracefully: bool,
    stop_ms: u128,
    killed: bool,
    /// Unix ms; lets the harness check that a signal reached only its service.
    requested_at_ms: u128,
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Default)]
struct RunOpts {
    method: Option<StopMethod>,
    stop_timeout: u64,
    hold: u64,
    state: PathBuf,
    inheritable_job: bool,
    allow_breakaway: bool,
    cgroup_root: Option<PathBuf>,
    shim: bool,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            method,
            stop_timeout,
            hold,
            state,
            inheritable_job,
            allow_breakaway,
            alloc_console,
            cgroup_root,
            shim,
            services,
        } => {
            #[cfg(windows)]
            if alloc_console {
                win::ensure_hidden_console()?;
            }
            #[cfg(not(windows))]
            let _ = alloc_console;
            let opts = RunOpts {
                method: Some(method),
                stop_timeout,
                hold,
                state,
                inheritable_job,
                allow_breakaway,
                cgroup_root,
                shim,
            };
            run(opts, services)
        }
        Cmd::Shim { ppid, cgroup, cmd } => {
            #[cfg(unix)]
            return unix::shim(ppid, cgroup, cmd);
            #[cfg(not(unix))]
            {
                let _ = (ppid, cgroup, cmd);
                anyhow::bail!("Unix only")
            }
        }
        Cmd::Cleanup { state, records } => {
            #[cfg(unix)]
            {
                let file = records.unwrap_or_else(|| state.join("services.json"));
                let recs: Vec<unix::Record> = serde_json::from_slice(&fs::read(&file)?)?;
                let killed = unix::cleanup(&recs);
                println!("{}", serde_json::to_string(&killed)?);
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = (state, records);
                anyhow::bail!("Unix only")
            }
        }
        Cmd::Breakaway { out } => breakaway(out),
    }
}

fn run(o: RunOpts, services: Vec<String>) -> Result<()> {
    let RunOpts {
        stop_timeout,
        hold,
        state,
        ..
    } = &o;
    let (stop_timeout, hold, state) = (*stop_timeout, *hold, state.clone());
    let method = o.method.unwrap_or(StopMethod::Break);
    fs::create_dir_all(&state)?;

    // ADR-0016: single instance through an OS lock on an open handle. The OS
    // releases it on any exit, so a stale lock cannot exist.
    let lock = File::create(state.join("lock"))?;
    if lock.try_lock().is_err() {
        eprintln!("locked: another instance owns {}", state.display());
        std::process::exit(3);
    }

    let mut started = Vec::new();
    #[cfg(unix)]
    let mut records = Vec::new();
    for spec in &services {
        let (name, cmdline) = spec
            .split_once('=')
            .context("service must be name=command")?;
        let log = File::create(state.join(format!("{name}.log")))?;
        #[cfg(windows)]
        {
            let job = win::Job::new(o.inheritable_job, o.allow_breakaway)?;
            let svc = match method {
                StopMethod::Conpty => win::spawn_conpty(name, cmdline, log, job)?,
                _ => win::spawn_break(name, cmdline, log, job)?,
            };
            started.push(svc);
        }
        #[cfg(unix)]
        {
            let opts = unix::Options {
                cgroup_root: o.cgroup_root.clone(),
                shim: o.shim,
            };
            let (svc, rec) = unix::spawn(name, cmdline, log, &opts)?;
            started.push(svc);
            records.push(rec);
            // Written as soon as anything runs: this is what a later instance cleans up.
            fs::write(
                state.join("services.json"),
                serde_json::to_vec_pretty(&records)?,
            )?;
        }
    }
    let _ = (
        &o.inheritable_job,
        &o.allow_breakaway,
        &o.cgroup_root,
        &o.shim,
    );

    // Let the trees grow (wrappers start their children), then record them.
    std::thread::sleep(Duration::from_secs(4));
    let pids: BTreeMap<String, Vec<u32>> = started
        .iter_mut()
        .map(|s| (s.name.clone(), s.pids()))
        .collect();
    fs::write(state.join("pids.json"), serde_json::to_vec_pretty(&pids)?)?;
    fs::write(state.join("supervisor.pid"), std::process::id().to_string())?;

    if hold == 0 {
        // Run until killed; the test kills us hard.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    std::thread::sleep(Duration::from_secs(hold));

    // Stop one service at a time so the harness can check that a signal for
    // one service does not reach the others.
    let mut reports = Vec::new();
    for svc in &mut started {
        let t0 = Instant::now();
        let requested_at_ms = unix_ms();
        let signal = svc.request_stop();
        let deadline = t0 + Duration::from_secs(stop_timeout);
        while svc.active_processes() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let gracefully = svc.active_processes() == 0;
        if !gracefully {
            svc.terminate();
        }
        reports.push(StopReport {
            service: svc.name.clone(),
            graceful_signal_ok: signal.is_ok(),
            signal_error: signal.err().map(|e| e.to_string()),
            stopped_gracefully: gracefully,
            stop_ms: t0.elapsed().as_millis(),
            killed: !gracefully,
            requested_at_ms,
        });
    }
    fs::write(
        state.join("report.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "method": method,
            "stops": reports,
        }))?,
    )?;
    Ok(())
}

/// Try to start a long-running child outside our job and report what happened.
fn breakaway(out: PathBuf) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let r = std::process::Command::new("ping")
            .args(["-n", "120", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
            .spawn();
        let text = match r {
            Ok(child) => format!("escaped {}", child.id()),
            Err(e) => format!("refused {e}"),
        };
        fs::write(&out, &text)?;
        println!("{text}");
        // Stay alive so the service has something to stop.
        std::thread::sleep(Duration::from_secs(600));
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = out;
        anyhow::bail!("Windows only")
    }
}
